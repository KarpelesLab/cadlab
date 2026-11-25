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
- [x] Thermal relief, third round (D119): what a pad is solid for as a stored attribute (`purpose`: press-fit,
      terminal, heat, stitching; `zone.pad --purpose`, `footprint.set --purpose`; KiCad's `pad_prop_heatsink`
      imported and exported); `board.rules --thermal-required` makes the thermal findings of the fab check
      errors that stop `fab.export` (off by default then; on since D120); `drc.starved_thermal` names what stands where the missing
      spokes would run, the tracks and vias as subjects (`thermal::blockers`); necks counted by their copper; a
      trimmed spoke no narrower than the minimum track; `zone.relieve` leads a track around what is in the way
      and takes zone layers, reliefs and joints from the fill before (`LayerMemo`, `JointMemo`), the same
      result as from scratch (`tests/zone_relieve.rs`)
- [x] Thermal relief required by default (D120, the user's decision of 2026-11-22): `thermal_required` is on
      for new boards, every preset, imported KiCad boards and saved projects without the field (only `false` is
      saved); the export's refusal and each finding name `zone.relieve`, `zone.pad --purpose` and `board.rules
      --thermal-required false`; a pad solid by wide tracks is let through by its own setting with a reason
- [ ] Thermal relief, left open (D119): the native router does not keep room for the spokes of relieved pads,
      and the native router moves no track off a pad's spoke on being told (`thermal::blockers` names them;
      the guide adapter does since D121, where its runs leave the time); a
      refill still costs the whole-layer booleans of each zone layer it changes, so `zone.relieve` is two to
      four fills, not one; no check of the copper on the two pads of a chip part against each other (no
      published number); the 3D view draws no copper; no repair for a pad that tracks drawn by hand or imported
      make solid (the routers' own tracks are laid no wider than the pad where they meet it, D121)
- [x] DRC: clearance, width, annular ring, drill, hole-to-hole, copper-to-edge, courtyard overlap, unrouted nets,
      silk over pads, keep-outs (`drc.run`, `src/drc.rs`); zone fills are checked like any copper
- [x] DRC: necks (`drc.neck`, D145): a track joined to another track, a pad or a via of its net by the overlap of
      round ends alone, where nothing else joins the two soundly (not other copper of the net, not a pour or a
      teardrop over it); an error under `min_track_width`, a warning under the width of what it joins; `fab.neck`
      in `fab.check`; `src/drc/neck.rs`, BOARD.md "Necks". The twelve corpus boards as their designers routed them
      have 2 errors (glasgow-revD1, two pads reached by 52 µm of copper) and 10 warnings
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
- [ ] `router: guide` level with the native router on the corpus, by the ranges of four runs of each (D117,
      with guide 0.0.21 and the native router on the same faster fill: level on buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2; ahead in every run on buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 1 to 24; cynthion 8 against 28 to 29; glasgow-revC3 4 to 7 against 31 to 36; ranges that overlap on corne-cherry 0 to 2 against 0 to 4; tinytapeout-demo 0 to 6 against 2 to 14; glasgow-revD1 84 to 120 against 99 to 169; behind in every run on none): the default is unchanged, and the switch is the user's. Done in D117: the reliefs of a fill kept from one fill to the next and made side by side (glasgow-revD1's fill 3.2 s → 1.5 s, 0.9 s after a few tracks moved; the same polygons, `tests/zone_refill.rs`), no fill at a run's end, the runs that move routes ended after two that say the board has no room, and the escapes of a run that moves no route planned round its routes (guide 0.0.21: what cynthion's run for shorter ways left open was a via cut by an escape planned again, not a pour). Still open for
      guide and its adapter: time for what comes after routing on the boards that a minute does not finish
      (the first run of tinytapeout-demo and the glasgows takes its whole share, 42 s of 60; track within a tenth
      of the native router's on five boards of twelve, on ten counted outside the pads; tinytapeout-demo +20 %),
      tinytapeout-demo's vias (372 to 427 against 243 to 290; ending its negotiation sooner cost connections,
      D117), glasgow-revD1 (84 to 120 open: a minute ends its first run, and five leave 14, all at its array), a
      track that leaves a net tie's pad within the tie (cynthion's eight: D116 has the geometry and D117 why no
      rule was changed), and rule areas with a routed track width (D121, "Where guide is still behind"; D118,
      "Gaps"; D117, "Gaps"; D116, "Gaps";
      D115, "Gaps"; D114, "Gaps"; D113, "Gaps"; D110, "Gaps";
      D108, "Gaps"; D106, "Gaps"; D104, "Gaps"; D101, "Gaps"; D97, "Gaps"; D96, "Gaps"; D95, "Gaps"; D93,
      "Gaps"; D91, "What got in the way"; D88, "What still blocks it"). Done since D126 (D127, the same
      direction, not met, six cells nearer): the items that several rounds had left. buspirate-flash-sop's
      via: guide 0.0.31, in its run for shorter ways, lays the route that cut a pad off its pour another
      way where it has one with no more vias that is not much longer, and takes out the via that joined
      the pad again (4 vias in three orders of four, as the native router has, and 6 in one).
      buspirate-rs232's track: a run ends on a pad where it comes to it, a track at the first copper of
      its pad, and a via costs a run that is searched for again 2 mm for 3 (8 vias and 284 mm for 5 and
      297 mm, beside the native router's 10 and 281 mm). The time: what a run spends was measured stage by
      stage on three boards, and the one stage that made nothing, the fills measured again against the
      whole board in every call (4.3 s of buspirate5-rev10's 25 s), is told what the fills are clear of
      (`Zones::fills_clear_of`, 1.6 s); a run that moves routes and comes back with more open is the
      last. By board and measure 30 better, 32 equal, 1 with ranges that overlap and 9 worse of 72,
      measured under a load of 3 to 10 (29, 27, 1 and 15 in D126); by the nets both routers complete 13,
      14 and 9 of 36 (13, 11 and 12). No pad without a neck on any board in any order; 583 connections
      open at 40 million nodes over the nine boards, as 0.0.30. Worse than 0.0.30: tinytapeout-demo's
      vias by some five of 280 and single orders of three boards at a budget of nodes, for the via at
      2 mm. Open (D127, "Not done"): corne-cherry's time, whose rounds cost what the native router's do
      and are four times as many at a price that every try to raise paid for in vias or connections;
      tinytapeout-demo's time on a quiet machine, 29 of whose 53 s come after its last connection;
      buspirate-flash-sop's sixth via in one order; corne-cherry at budgets of nodes; and another
      round's items, the planning of a net with a plane on outer pours and the glasgows' track. Done since D125 (D126, the same
      direction, not met, and no nearer by the count of the cells): a pad left with no spoke by 0.0.29
      (a ground pin of a header on tinytapeout-demo, in one order of sixteen) traced to the run for
      shorter ways and to the step for the spokes, and closed three ways: guide 0.0.30 keeps a pad's last
      spoke as copper of its net in those two runs (`RouteOptions::kept_spokes`), a shorter way that
      leaves a pad that had a neck with none is not kept, and the step for the spokes is kept by the pads
      with no spoke first, a try at a time from what the last one kept. No pad without a neck on any
      board in any order measured (52 runs at 60 s, and tinytapeout-demo's sixteen orders before the
      release). And a fault of guide's own, found on the way: a net that it routed again was kept across
      another net's track, and cadlab's check took the pieces out (tinytapeout-demo: up to 323 tracks and
      vias in three orders of eight at 150 million nodes, none with 0.0.30). By board and measure 29
      better, 27 equal, 1 with ranges that overlap and 15 worse of 72, measured on a quiet machine (26,
      29, 3 and 14 in D125 under its load; 29, 29, 1 and 13 in D124); by the nets both routers complete
      13, 11 and 12 of 36 (13, 10 and 13). Worse than 0.0.29: one connection open in one order of eight
      of tinytapeout-demo at 150 million nodes for none, and two or three vias in single orders of two boards
      at a budget of nodes. Open (D126, "Not attempted"): all four of the direction's other items, the
      planning of a net with a plane on outer pours with the routes made legal first, the glasgows'
      track, corne-cherry's time, buspirate-flash-sop's via and buspirate-rs232's track, the last two
      looked at by net. Since D126 (D128, the same direction, not met): the first of those built in guide
      0.0.32, the routes made legal before the fill in the native router's order (`legal_pours`) and a net
      with a plane planned on its outer pours (`outer_pours`), and measured stage by stage: the first
      round's difference is closed and is gone by the end of the run, glasgow-revD1 has 42 to 71
      connections open with it for 12 to 17, and the order alone gains nothing; both are left off. Since
      D129 (D130, the same direction, not met): the native router's negotiation itself in guide, whole,
      as a first stage that a host can ask for (guide 0.0.34, `Rounds::Native`), which ends its rounds as
      the native router does, round for round on corne-cherry and buspirate5-rev10; with guide's passes
      after it tinytapeout-demo has 5 % fewer vias and 6 % less track and four to seven connections open
      for none or one, buspirate5-rev10 as much less in half as much time again and lumenpnp-mobo more of
      both, so it is left off; open: the pads that the stage's routes shut in, and its time.
      Since D146 (D147, the same direction, not met): guide 0.0.50. The necks are mended where they are made
      (two stubs that touch by their round ends are two terminals to guide and get their piece; a route that ends
      on a teardrop or a pour's edge beside its pad goes on into the pad; `drc.neck` counts a fill that joins two
      pads), and the last step lays no piece of track in 52 runs of guide's (20 in 11) nor in 192 (72 in 32); the
      native router's 3 in 52 are as they were. The machine's pace is measured in the run (`router::pace`) where
      D146 had a constant. What a run of 60 s repeats is measured: one board in 22 of 24 board-orders over eight
      repeats, the two others glasgow-revD1's; the clock's decisions are listed by board with their margins. The
      deadline asked at marks of the run's own work was built and measured and is not kept (the same count of
      distinct boards, one board in 19 of 24, glasgow-revD1 at half speed 37 open for 6 to 9). 35 cells better, 30
      equal and 7 worse of 72, as master's build. Open: the seven cells; the runs that end at their budget are
      the clock's (a count made before the run needs a second in nodes by stage and board); the native router's
      track past a pad by its flank; corne-cherry's time and tinytapeout-demo's rounds, not begun.
      Since D145 (D146, the same direction, not met): guide 0.0.49. The clock no longer says which round of the
      native router's rounds in guide is the last: the adapter gives the negotiation's share as a count of nodes
      made from the budget, asked about between two rounds, and tinytapeout-demo's ninth order is one board in
      eight runs for five, its sixteen orders the same boards in three sets for 6 of 16. The rounds end three
      rounds after their fewest shared points, not at the first round with few: the tenth order's mng6, open in
      every set since D143, is routed, and the sixteen orders have no connection open in three sets (7, 2 and 5
      with 0.0.48 in the same minutes). 35 cells better, 30 equal and 7 worse of 72 (master's build 35, 29, 8).
      Open: the seven cells; the necks at their source (they are in guide's first run, at a route's end beside
      its pad; the mending still lays 20 pieces in 52 runs); tinytapeout-demo's track outside the pads, which is
      in the negotiation's routes; the clock's decisions in the adapter's later runs and in guide's own rounds.
      Since D144 (D145, the same direction, not met): the DRC has the necks (`drc.neck`), and with it the twelve
      boards in four orders had 894 on guide's boards with 0.0.47 (678 under the minimum track width; 884 a track
      that reaches a pad by its tip, which D144's script did not count) and 2 on the native router's. guide 0.0.48
      tells no last piece of a route into its pad needless for the round end of the piece before it, the
      adapter's check of that step puts tracks back at new necks, and a last step of either router lays a piece of
      track across what is left (`router::necks`): no neck in 52 runs of guide and 52 of the native router, for
      0.01 to 0.4 % more track at budgets of nodes with every run's connections as they were. tinytapeout-demo's
      tenth order measured in guide: the cheapest way for its connection across other routes crosses 9 to 12
      nets, and with the nets round its pads taken out first one of them has no way back; not a pin's last
      escape, nothing kept. Beside the native router 35 cells better, 30 equal and 7 worse of 72, as master's
      build in the same minutes; tinytapeout-demo over three sets of sixteen orders 11 connections open for 6,
      by first runs that took their slow way (the same code in both builds; repeated, both have them). Open: the
      seven cells; the tenth order; which pass of guide lays the stubs that end beside a pad; the track outside
      the pads of tinytapeout-demo by net, not begun.
      Since D143 (D144, the same direction, not met): guide 0.0.47, no track between pads told needless for what two
      round ends leave of it: the step that takes out the tracks that a pour joins left 122 necks under 0.1 mm on the
      twelve boards (corne-cherry 89, the narrowest 25 um of a track of 0.25 mm; joined by the DRC's count), and
      leaves 4, for 0.4 % more track on corne-cherry and 0.6 % on glasgow-revC3. Counted before anything was built:
      the passes on more than one thread (1.01 to 1.05 times on three for an ideal look-ahead, where 1.4 was asked),
      the eights chosen apart (slower with no net routed again). Measured: what each second of corne-cherry buys (23 s
      of its slowest order nothing; the signal for the passes on the finer lattice holds in 48 runs and is set from
      five on two boards), tinytapeout-demo's open connections by what each is (the tenth order's a pin of the QFN
      shut in by a via's clearance, in eleven sets of eleven; the others pads of pours that come with the load on the
      first run's share), the nets that the last run leaves as given (in pieces by tracks taken out between theirs,
      not by their teardrops). Beside the native router 35 cells better, 30 equal and 7 worse of 72 for master's 36,
      29 and 7 in the same minutes (lumenpnp-ringlight's track outside the pads equal for better, by 0.6 mm of a step
      that stays). Open: the seven cells; the tenth order's pin; a count of necks in the DRC or in the table, which
      neither has.
      Since D142 (D143, the same direction, not met): guide 0.0.46, the native router's rounds in guide with less
      round each search and the same routes to the node (whether two sides can meet told by the first of each, the
      claims of a piece on a slant made from the rows near it): tinytapeout-demo's rounds 24.1 s for 27.1 s,
      lumenpnp-mobo's run 27 to 30 s for 30 to 34 s. The check of the last run tells the nets on the old routes of
      those that go back by their new vias as by their new tracks, at the clearance of the nets' classes, and the
      nets with teardrops alone are handed to that run (corne-cherry 25 to 45 tracks fewer of 2400). Built and not
      kept: the nets of a round routed ahead of their turn on the other threads, kept where their searches read
      nothing that changed; the same routes on one, two, three and eight threads, 29 to 39 s for 27 s. Beside the
      native router 37 cells better, 28 equal and 7 worse of 72 for master's 36, 28 and 8 in the same minutes (36,
      29 and 7 in the set before): tinytapeout-demo's vias and glasgow-revC3's time change sides by the runs.
      tinytapeout-demo's thirteenth order has no connection open in five sets, its first run ended on its own in
      the three of this build. Open: the seven cells, corne-cherry's time first (a first run of 35 s in its slowest
      order, of which 11 s are passes on one thread); the threads of a first run (1.7 to 1.8 of three in guide's
      rounds, one elsewhere); one connection open in three to five of tinytapeout-demo's sixteen orders; the 105
      of corne-cherry's nets with teardrops that guide leaves as given.
      Since D141 (D142, the same direction, not met): guide 0.0.45, two options for what a last run did for nothing
      that this adapter reads (every connection that guide's own count leaves open explained by a search, 436 on
      glasgow-revD1; a search for fewer vias for the nets handed with their tracks ended short of a pad's centre,
      kept where the run has the time, as lumenpnp-mobo's 324 vias for 331 come from it). The nets whose
      straightened tracks part a pour are told by the fill of the two pieces that the open connection's ends are on
      and have their old routes back alone, and the last run's time is kept by what it takes. The run is kept in 59
      of 60 runs for master's 52 in the same minutes, the step for the tracks between pads is made in 8 of 8 runs of
      the two glasgows for 6 (5 in the set before), glasgow-revD1 has 1.4 % less track for it, and its last run is
      0.46 s in guide for 1.77 s. Beside the native router 36 cells better, 29 equal and 7 worse of 72 for master's
      35, 28, 1 overlapping and 8 in the same minutes (37, 27 and 8 for 36, 27, 1 and 8 in the set before):
      tinytapeout-demo's vias change sides by the runs, and no cell moves by this. Measured and not kept: a net with
      a long way round laid again with the nets in its shorter way (one try in thirty to a hundred kept, 1 to 14 mm
      of a board); the native router's ways for the twenty nets with the most excess on glasgow-revD1 (free on
      guide's board in none of 80, a net that only guide completes in 76); an end for corne-cherry's runs that move
      routes (its slowest order is a first run of 36 s). Open: the eight cells; tinytapeout-demo's thirteenth order;
      corne-cherry's last run, which has 3 of its 151 nets (the others have teardrops, zones of their nets); the
      time that glasgow-revD1 leaves of its minute (4 to 5 s).
      Since D139 (D140, the same direction, not met): guide 0.0.44, with polyclip 0.0.7. `neck_at_pads` leaves no
      track narrower that then no longer touches what it touched of its net, and this adapter narrows before its
      report (a track that reached its pad by its round end alone left 5 µm short of it with a report that had it
      joined). guide's two things of D139 are released behind options and asked for here: the stairs laid straight
      in a last run of its own, after every step that moves a route (2 to 6 % of the tracks of the nets both builds
      complete, the connections and the pads short of two spokes the same in 28 of 28 pairs of runs), and the nets
      round a pad that the routes shut in laid another way for its stub, the nets that the stub's cheapest way
      across the routes runs into first (tinytapeout-demo over sixteen orders in six sets: 31 connections open for
      58, fewer in every set). Beside the native router 36 cells better, 28 equal, none overlapping and 8 worse of
      72 for master's 35, 30, none and 7 in the same minutes (tinytapeout-demo's vias, at the script's edge, by the
      runs), and 4 to 10 % fewer tracks than master's on the nets both routers complete. Open: the eight cells
      (corne-cherry's time; tinytapeout-demo's vias and track outside the pads; the two glasgows' track, and
      glasgow-revD1's vias, which are the nets that only guide completes); tinytapeout-demo's thirteenth order; the
      last run on a board whose pours its tracks cut, and its time in stages of guide that have nothing to do.
      The fills since D139 (D141): polyclip 0.0.7, whose offsets compute the union of their raw curves only where
      it is not one already computed (clusters by the distance of their edges, a cluster that is another one moved
      computed once, a memo of the results of clusters that `geom` holds for the process): the fills of a run 12 to
      51 % cheaper by board (cynthion 17.4 s for 9.2 s on one thread) and the same to the nanometre on the eleven
      boards with zones. Open: the edges of a few nanometres that offsets leave, and the trim at concave vertices
      (another 15 % of an opening, a change of the output).
      Since D138 (D139, the same direction, not met): guide stays 0.0.42. `board::zones` keeps the stages of a zone
      layer's fill from one fill to the next (the zone's area on the board, the pour before its islands are told,
      the empty fills, the parts grown as keep-aways): the fills of a run are 13 to 58 % cheaper by board and the
      same to the vertex over 9918 zone-layer fills; what is left is polyclip's offset with round joins. Built in
      guide and not released (its pull request 85): the nets round a pad that the routes shut in laid another way
      for its stub, which joins 17 of 30 nets with such a pad and leaves tinytapeout-demo's count over sixteen orders
      where it was (24 for 30 in four sets, 13 for 12 in the two at the lower load); and the steps of stairs laid
      straight, a tenth of the tracks of the nets both routers complete, which loses corne-cherry a connection in
      one order of four and left one run of 41 with a connection open more than its report said (this adapter
      narrows its tracks at the pads after its report). Beside the native router 35 cells better, 29 equal, none
      overlapping and 8 worse of 72 with this and with master's build in the same minutes, for 34, 31, none and 7:
      tinytapeout-demo's vias worse for equal by the runs. Open: tinytapeout-demo's pads that the routes
      shut in; what each of the two would take (a way for the pad where the second run has none either;
      `straighten` in the last run alone and the tracks narrowed before the report); the track of the two glasgows
      and tinytapeout-demo's vias on the nets both complete; corne-cherry's time; polyclip's offset.
      Since D137 (D138, the same direction, not met): guide 0.0.42. The round that joins what the routes cut off the
      fills to them again is made also when the deadline is past, where a run whose passes had all of its time
      returned such pads cut off and counted joined; and the step for the spokes keeps the nets that guide found no
      way for round their rooms as they were: kept in 6 tries of 16 on glasgow-revD1 for none of 20, a pad or two
      fewer short of two spokes each. Every run at a budget of nodes is 0.0.41's. Measured and not changed:
      tinytapeout-demo's open connections are pads of nets with pours that the routes shut in, in runs cut by the
      deadline and not, which no run after the rounds closes (the order with 3 open has 3 still); a round of the
      joining after cleanup and a run that moves only the routes near what is open, neither a gain; the track
      outside the pads by net (on tinytapeout-demo the nets with more vias as well, on the glasgows spread, nothing
      at the pads); corne-cherry's settling is routing. Beside the native router 34 cells better, 31 equal, none
      overlapping and 7 worse of 72, for 33, 30, one and 8, the three cells that moved being the runs' and the
      native router's. Open: tinytapeout-demo's pads that the routes shut in; the track of the two glasgows and
      tinytapeout-demo's vias on the nets both complete; corne-cherry's time; the fills.
      Since D136 (D137, the same direction, not met): guide 0.0.41. A run that cleanup pulls tight goes no nearer a
      pad across the gap of its spoke, and after guide's own rounds cleanup has a quarter of a deadline's time for a
      twelfth: glasgow-revD1 at 60 s has 10, 12, 14 and 10 pads short of two spokes for 8, 23, 23 and 22, 2 % fewer
      vias and 10, 17, 8 and 13 connections open for 12, 17, 4 and 12 with 0.0.40 in the same minutes; every run at a
      budget of nodes has the connections open that it had. Measured and not changed: glasgow-revD1's track on the
      nets both complete (+8 % at 60 s, +5 % at 240 s, spread over the nets), tinytapeout-demo's vias (the nets that
      go under a bundle and back; 259 to 303 vias by the run), corne-cherry's time (its run for shorter ways is
      5.7 s for a thousandth of its track, and its default order's first run ends at its deadline all the same).
      Beside the native router 33 cells better, 30 equal, one overlapping and 8 worse of 72, for 36, 28, none and 8.
      Open: the track of the two glasgows and tinytapeout-demo's vias on the nets both complete; tinytapeout-demo's
      first run ended by its deadline before its fills are joined again (one run in sixteen with 3 open);
      corne-cherry's time; the step for the spokes on glasgow-revD1, kept in none of 20 runs; the fills.
      Since D135 (D136, the same direction, not met): guide 0.0.40, speed with no result changed, on the threads. A
      net's search for a route with fewer vias is made ahead of its turn on the other threads, in a copy of the
      board, and stands if no net before it ended with other copper, as 92 to 97 in a hundred do not; the nets of a
      round are begun the longest first. The same routes and nodes on one thread and on three with an hour for a
      budget, each of 43 runs at budgets of nodes as 0.0.39 has it, and at 60 s the same board 1 to 4 s sooner where
      a run ends on its own; tinytapeout-demo out of its budget in one order of sixteen for six. Measured and nothing
      gained: the search of guide's passes (four changes to a node of it, no time), the fills (each of another board;
      the pours over the whole board made again with any track), this adapter between the runs (its fills). Beside
      the native router 36 cells better, 28 equal, none overlapping and 8 worse of 72, for 34, 28, 1 and 9 (the same
      with 0.0.39 in the same minutes). Open: corne-cherry's time (28 to 54 s for 21 to 32 s; 1, 0, 4 and 1 open at
      a budget of 40 s), whose settling and cleanup are one thread; the track in the open of the two glasgows and
      tinytapeout-demo; the passes and the native router's rounds in guide on one thread; the fills.
      Since D134 (D135, the same direction, not met): guide 0.0.39, speed with no result changed. A profile of the
      minute put 31 % of glasgow-revD1's processor time in the question whether an end of a connection is walled in,
      asked of 20 000 nodes in a wandering order, and a quarter of a search of guide's own rounds in its heap; the
      first is now asked from the nodes nearest a source and the heap has four items under each with a rank and its
      node as one number: the rounds 1.27 and 1.38 times as fast on corne-cherry and glasgow-revD1, whole runs at
      budgets of nodes 1 to 8 % (the target was 1.5 times), each of 43 such runs as 0.0.38 has it. Measured and
      nothing changed for it: guide's track is not longer at its pads (within 2 mm of them as much as the native
      router's or 6 to 7 % less) but in the open, where the nets that only guide completes cost a part of it
      (glasgow-revD1: guide on the native router's nets alone -1.8 to +3.9 % of track for +6.5 to +8.4 %); and
      corne-cherry at a budget of 30 s has 9, 0, 3 and 6 connections open for the native router's 1, 3, 2 and 4, so
      no end by diminishing returns. Beside the native router 34 cells better, 28 equal, 1 overlapping and 9 worse
      of 72, for 35, 29, 1 and 7 (35, 29, none and 8 with 0.0.38 in the same minutes): lumenpnp-mobo's two by the
      native router's own runs, tinytapeout-demo's third order with either release, and its vias at the rule's edge.
      Open: corne-cherry's time (29 to 54 s for 20 to 32 s), the track in the open of the two glasgows and
      tinytapeout-demo, the time of guide's passes.
      Since D133 (D134, the same direction, not met): guide 0.0.38, in which a negotiation that ended with
      one point shared is not begun again on other grids (nrfmicro's slowest order 15 s for 21 s, the board
      as it was in the two orders of eight that had such tries), and in the adapter a run that moves routes
      ended sooner, for the step that takes out the tracks between pads that a pour joins to have its time
      (glasgow-revC3: skipped in two or three orders of four, now in none). Measured and dropped: the native
      router's rounds on two layers (corne-cherry 477 vias and 8140 mm for 439 to 453 and 7470 mm, with the
      teardrops as fills alone 491 and 8196 mm; nrfmicro a connection open in two orders of four), a pass for
      the long ways round on glasgow-revD1 (46 to 169 mm in a second that its first run does not have), the
      stage's pass for fewer vias repeated (1.3 % of tinytapeout-demo's vias for 4 to 7 s). Beside the native
      router 35 cells better, 29 equal, 1 overlapping and 7 worse of 72, for 36, 27, none and 9: nrfmicro's
      time equal for worse; tinytapeout-demo's vias equal for worse at the rule's edge, with either release;
      corne-cherry's connections open overlapping for better, a run's variance with either release. Open:
      corne-cherry's time (its rounds, 18 to 24 s for the native router's 10), glasgow-revD1's track and vias
      (the price of the connections its rip-up makes), glasgow-revC3's and tinytapeout-demo's track outside
      the pads.
      Since D132 (D133, the same direction, not met): the report made of the board that leaves, with guide
      0.0.37. The tracks were ended inside their pads after the report, and a track that was the only copper
      between its pad and something else of its net went: lumenpnp-mobo's two pads apart, one of
      lumenpnp-ringlight's, and 5 to 8 connections of corne-cherry that the report did not have; now none of
      60 runs at 60 s and 43 at budgets of nodes has a count other than the DRC's, and open is the DRC's count
      for both routers in the tables. The stage's rounds brought to their end sooner in guide (a point that
      stays shared at 2 a round for 0.5), their pass for fewer vias given its time, the look's maps the
      stage's, and the first run 0.78 of the budget with the stage: tinytapeout-demo 282.5 vias for 300.5 and
      3 connections open in eight orders for 10, for 1.6 % more track; guide's own rounds 288.5, the native
      router 273. Beside the native router 36 cells better, 27 equal and 9 worse of 72, for 34, 27, 1
      overlapping and 10 counted the same way. A class worse: tinytapeout-demo's track (equal) and its track
      outside the pads (worse). Open: tinytapeout-demo's vias (+3 %) and track outside the pads, the time of
      nrfmicro and corne-cherry (measured, two rules tried and dropped), two layers, the glasgows' track.
      Since D131 (D132, the same direction, not met): the stage adopted for a run with a budget on a board
      of more than two layers, with guide 0.0.36: the board looked at before the run plans anything for the
      stage, and one given up routed as if the stage were never asked for (to the track at a budget of
      nodes, 1.3 to 1.9 s later at 60 s for 4 to 5 s); a route that begins on another pad of its net handed
      over as that pad's, which was buspirate5-rev10's connections lost after its first run; no run for
      shorter ways after the stage. Beside the native router 35 cells better, 28 equal, 1 overlapping and
      8 worse of 72, for 31, 29, 2 and 10: tinytapeout-demo's track and track outside the pads and
      buspirate5-rev10's vias and track outside the pads a class better, none a class worse; buspirate5-rev10
      in 18 to 24 s and lumenpnp-mobo in 33 to 36 s. Worse than with the stage off: tinytapeout-demo's vias,
      299.5 for 287.5 (273 with the native router), its first run ending at its deadline; and two pads of
      lumenpnp-mobo apart on a board reported whole. Open: tinytapeout-demo's first run, those two pads,
      two layers, the glasgows' track, corne-cherry's time.
      Since D130 (D131, the same direction, not met): where the stage's open connections come from,
      stage by stage, and five changes to it in guide 0.0.35 (what is still shared taken out a wire at a
      time, the price of a spoke's room and the last spoke kept, no net planned on a pour of an outer
      layer, the rounds for the pours an option, a board given up before its first round): with guide's
      passes after it tinytapeout-demo has as many connections open as with guide's own rounds, 5 % less
      track and 3 % more vias, and buspirate5-rev10 4 % fewer vias and 3 % less track, later; the boards
      that the stage gives up lose 4 to 5 s of the minute, glasgow-revC3 its lead in vias, so it is left
      off; open: the stage's look at the board before the run's escapes, its time, and two layers.
      Since D128 (D129, the same direction, not met): the connections that the negotiation leaves open
      negotiated for on the grid of the pitch before the passes (guide 0.0.33, `whole_finer`), measured
      stage by stage: it halves what the passes are given and the nets they take out, and the board ends
      as it did, so it is left off; the round's table on a quiet machine, 29 better, 30 equal, 2 that
      overlap and 11 worse of 72 cells, and by the nets both complete 13, 14 and 9 of 36; glasgow-revD1
      lost no connection from 0.0.30 to 0.0.32 (the three side by side); glasgow-revC3's track is level on
      the nets both complete, and glasgow-revD1's is in the nets taken out as its share ends the rounds,
      the twenty longest of them 374 to 482 mm, half of it gone at 240 s; corne-cherry with `legal_pours`
      has twelve connections open over eight orders for nine. Open (D129, "Not attempted"):
      tinytapeout-demo's vias and track, which are not made by the passes; rounds that end in their share
      on six layers; corne-cherry's time; the node cost of guide's rounds at the first pitch. Done since D124 (D125, the same
      direction, not met): the first round of both routers joined connection by connection and the
      differences counted by their kind (the planning of a net with a plane, the room of a spoke, the
      order, the tree, where a route ends at a pad, and the widths of the nets); the native router's rule
      for the width of the nets of a row of pads of fine pitch in a rule area with a clearance of its own,
      and a lattice of their pitch (guide 0.0.28 and 0.0.29: tinytapeout-demo's vias +3 % for +10 %, its
      track +5 % for +10 % and +8 % for +13 % outside the pads; beside 0.0.27 under the same load 275.5
      vias for 309 and 5059 mm for 5318). By board and measure 26 better, 29 equal, 3 with ranges that
      overlap and 14 worse of 72 (29, 29, 1 and 13 in D124), measured on a machine at a load of 10 to 34
      where D124 had 2 to 5; by the nets both routers complete 13, 10 and 13 of 36 (14, 10 and 12). Worse
      than 0.0.27: one pad without a neck on tinytapeout-demo in two orders of eight, one of them a pin
      of the ground in a header that the narrower nets now pass beside, not mended; 0.0.28 itself on
      corne-cherry, which 0.0.29 is for. Open (D125, "Not attempted"): the planning of a net with a plane
      on the pours of the outer layers, which needs the native router's order of rounds, legal routes and
      fills; glasgow-revD1's and glasgow-revC3's track, there after guide's first round at twice the
      pitch; corne-cherry's time; the step for the spokes a room at a time; buspirate-flash-sop's via
      and buspirate-rs232's track. Done since D123 (D124, the same
      direction, not met): a net of two pads that has no way on the board told once, by the search for it
      whole with no route on the board and by a search across every route, and no search of the whole
      board for it in each pass; the time of the step for the spokes kept from the first run on, and the
      tracks between pads taken out after it; each net routed again before the fills are joined again
      (guide 0.0.26 and 0.0.27: cynthion 1, 1, 1 and 1 pads short of two necks for 5, 5, 1 and 1 and none
      without a neck on any board, in 39 to 44 s for 56 to 59 s; glasgow-revC3's track +6 % for +8 %). By
      board and measure 29 better, 29 equal, 1 with ranges that overlap and 13 worse of 72 (27, 29, 1 and 15
      in D123): vias on three boards, track on four and outside the pads on the same four, time on two; by
      the nets both routers complete 14, 10 and 12 of 36 (13, 11 and 12). Worse than 0.0.25: one or two
      connections more open on glasgow-revD1 and glasgow-revC3 (the twelve boards 28 to 35 for 27 to 32),
      for the time kept for the spokes; 0.0.26 at a budget of nodes, which 0.0.27 is for. Open (D124, "Not
      attempted"): glasgow-revD1's track (+20 %), which D123 made worse and this round did not mend; why
      guide's negotiation settles less than the native router's; corne-cherry's time; the step for the
      spokes on glasgow-revD1, kept in no order. Done since D122 (D123, the same
      direction, not met): each net routed again where the others are by the negotiation's search, once
      every route is where it is, with claims for copper that is off its grid; the native router's gridless
      pass in the run for shorter ways; a plane at no price in guide's negotiation and the native router's
      forty rounds at twice the pitch; no pass on the finer lattice, and no run that moves routes, after one
      that moved no net (guide 0.0.25: the twelve boards 27 to 32 open against 179 to 218 for 43 to 58,
      glasgow-revD1 7 to 15 for 22 to 39; tinytapeout-demo's vias +13 % for +32 % and its track +7 % for +16 %;
      sweep-v2.2 40 to 42 s for 58 s, under the native router's). By board and measure 27 better, 29 equal, 1
      with ranges that overlap and 15 worse of 72 (24, 29, 2 and 17 in D122): vias on four boards, track on
      four and outside the pads on the same four, time on two, cynthion's pads short of two necks; by the nets
      both routers complete 13, 11 and 12 of 36 (11, 10 and 15). Worse than 0.0.24: glasgow-revD1's track
      (+18 % for +11 %; +10 % for +5 % on the nets both complete), cynthion's pads short of two necks in the two
      orders that leave the spoke step no room. Open (D123, "Not attempted"): the vias of a round at twice
      the pitch that no net loses alone, glasgow-revD1's track, corne-cherry's time and its connections at a
      budget of nodes, the passes on the finer lattice for connections with no way. Done since D121 (D122, the same
      direction, not met): a via of a net with a pour at the native router's price on two layers (guide 0.0.24:
      corne-cherry 430 vias against the native router's 459 for 504, lumenpnp-ringlight 3 against 3 for 4);
      of two ways as long as each other the one with less outside the pads; the runs that move routes ended
      where one has said all there is (cynthion 49 to 57 s for 57 to 59 s, its track level with the native
      router's, the spoke step begun in two orders of four); the first step for the needless vias made
      whatever the time. By board and measure 24 better, 29 equal, 2 with ranges that overlap and 17 worse of
      72 (21, 28, 3 and 20 in D121): vias on four boards, track on four, outside the pads on six, time on
      three; by the nets both routers complete, tinytapeout-demo +34 % vias and +18 % track, glasgow-revD1 +6 %
      and +5 %, glasgow-revC3 -11 % and +5 %. Open (D122, "Not attempted"): the vias and track that guide's
      negotiation leaves a net with under another (it ends its rounds on tinytapeout-demo with 458 vias where
      the native router ends its own with 255), the native router's gridless pass, sweep-v2.2's and
      corne-cherry's time. Done since D118 (D121, with the
      direction that guide end equal to or better than the native router on every measure on every board,
      which it does not): a negotiation at the first pitch that does not end begun again on finer grids, as
      the native router's whole run is (guide 0.0.23: nrfmicro none open in any order, 11 over twelve orders
      before); a via to its plane at the native router's price for a net with one (glasgow-revC3's track +14 %
      over the native router's for +19 %); a track of either router laid no wider than the pad it meets
      (`neck_at_pads`: no pad solid by its tracks on the corpus or in the session replays); routes moved out of
      the room of a spoke of a pad short of its spokes, where the runs leave the time (kept in half its tries).
      By board and measure against the native router, 21 better, 28 equal, 3 that overlap and 20 worse of 72
      (19, 29, 2 and 22 in D118): vias on six boards, track on five, time on three. Done since D117 (D118): guide's
      negotiation routes the nets of a round eight at a time on its threads with the same routes on any number,
      keeps a search round its two ends late, and the balls deep in an array whose nets' own via fits have their
      vias planned (guide 0.0.22: glasgow-revD1 39 to 150 open at a minute for 84 to 120, 2 at five minutes for
      14; tinytapeout-demo none open in four orders); the tracks end where they are inside the pads of their net
      (`pad_ends`: track within 2 % of the native router's on eight boards of twelve); freerouting 2.5.0 beside
      guide 0.0.22 on the CI runner. Done since D115 (D116): the ways out of
      the second ring of an array of balls planned before anything is routed and the dog-bones of the balls
      further in away from its middle (guide 0.0.20: glasgow-revC3 5 to 7 open for 29 to 34); the tracks from
      pad to pad that a pour joins taken out by the fill of the routed board, where no connection opens and no
      pad is left short of its spokes for it (`pcb::needless_between`, `thermal::joints_of`: lumenpnp-mobo 520 to
      552 mm, buspirate5-rev10's track +5 % on the native router's for +17 %); a run of guide's for shorter ways
      once the others are over, where they leave the time (`RouteOptions::shorten`); corne-cherry's vias within
      15 % of the native router's (502.5 against 464.5 by the medians, the price of a shared point stopping at
      thirty steps on two layers); `route_bench` tells the track outside the pads of its net and writes a
      result's nets and tracks (`CADLAB_BENCH_BY_NET`). Done since D114 (D115): guide's negotiation
      for every net on a grid of twice the pitch on boards of more than two layers (guide 0.0.19: glasgow-revD1
      is negotiated, the vias of the boards of four layers come down); every via that the fill of the routed
      board makes needless taken out by that fill, with none made for it (`pcb::needless`, `needless`); the heat
      pads named as pads that vias of their net may be in where no track leaves them (`Zones::via_pads`:
      cynthion's three exposed pads joined); the fill at a run's end given only before its deadline. Done since D113 (D114): cadlab's fill of
      the zones around guide's routes given to guide when it asks (guide 0.0.18, `RunControl::refill`,
      `host_fills`), by which it tells the pads cut off their pours (corne-cherry 0 to 2 open for 5 to 10);
      a wire of its negotiation that ends where the fill is no more routed again; a negotiation that ends with
      points shared begun again in another order (nrfmicro 0 for 0 to 1); the stubs to a plane that a pour
      makes needless taken out by the fill guide is given at the end of each run, kept on the board between the
      runs and taken out once they are over. Done since D110 (D113): every net routed against every other at once on one grid before
      guide's passes, as the native router's negotiation does (guide 0.0.17, `RouteOptions::whole`: corne-cherry
      5 to 10 open for 15 to 21, nrfmicro 0 to 1 for 0 to 2 and closed in 7 to 18 s in three orders of four, the
      eleven boards without glasgow-revD1 55 to 59 for 70 to 84); the spare spokes and the number a pad keeps
      given to guide; the first pass on six layers timed call by call. Done since D106 (D108): the D+ of a USB receptacle whose pads have those of D- between them
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
- [x] ems 0.0.18 (D111): the loss that moved with the time step was the absorbing layers' convolution, first
      order in the step (a layer whose conductivity grew with the step: 0.082 dB/ps on a 60 mm line at 9 GHz),
      second order now, the 64 mm line 0.082 dB from D52 for 0.090; a run goes on past its energy criterion
      while the ports' signals still add to the lowest frequency (`settle`, on by default, twice the steps at
      most): the 0.06 GHz points 0.0008 and 0.0011 dB for openEMS's 0.0053 and 0.0035, the capacitor 0.0029
      for 0.0058. Left: openEMS nearer on the parts against ideal 50 Ω lines and by 0.1 % on a Z₀ read where
      the ports decide it; the via takes 6 % more steps than it did

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
