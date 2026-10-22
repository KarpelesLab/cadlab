# Testing and verification

EDA output errors are expensive: a bad Gerber costs a board run. cadlab checks itself in layers, from fast unit tests
up to comparison against independent tools.

## Layers

| Layer | What | Where |
|---|---|---|
| Unit + property tests | geometry, units parsing, model invariants (`proptest`) | every crate |
| Golden/snapshot tests | exporters, renderers, summaries (`insta`); byte-exact thanks to determinism | `tests/golden/` |
| Fuzzing | all parsers (KiCad, DSN, Gerber reader, unit strings), polygon ops | `fuzz/` |
| Round-trip | save→load→save identical; export→import→compare for interop formats | `tests/roundtrip/` |
| **Oracle tests** | compare cadlab results with independent external tools | `tests/oracle/` |
| Agent scenarios | scripted MCP sessions designing a board from a prompt | `tests/agent/` |
| Benchmarks | router results and timings of load, DRC, zone fill, render, export against committed baselines (see "Benchmarks in CI") | `tests/route_bench.rs`, `tests/perf.rs`, `tests/golden/bench/` |

## Oracles

External tools used **only as verification oracles**: run as separate processes in tests/CI, never linked, never
vendored, never a runtime dependency, and none of their code copied (cadlab is MIT; see DECISIONS.md D7).

| Oracle | Checks |
|---|---|
| **KiCad** (`kicad-cli`) | export board to `.kicad_pcb` → KiCad DRC vs cadlab DRC; KiCad Gerber export vs ours; ERC on exported schematic; netlist equivalence |
| **freerouting** | same DSN routed by both → compare completion, via count, length, runtime (router benchmarks) |
| **gerbv** (and/or other Gerber viewers) | our Gerbers parse cleanly and rasterize to the expected image |
| **Clipper2** | differential testing of the polygon library |
| **ngspice** | SPICE netlist export parses and simulates: `ngspice -b` on the divider golden file gives v(out) = 2.5 V (`tests/electrical.rs`, M8) |
| **openEMS** (optional; CI job "openEMS oracle", informative) | the exported full-wave model runs (`python3` with openEMS's Python modules): a lossy microstrip (Djordjevic–Sarkar laminate, 58 MS/m copper) exported and run by openEMS agrees with `sim.sparams` on the same options (\|S21\| within 15 % + 0.025 dB, Z₀ within 5 % of it and of the 2D field solver; ELECTRICAL.md "Full-wave export") (`tests/openems.rs`; skipped when `openEMS` is not installed; the 4-layer via transition's run is opt-in, `CADLAB_OPENEMS_VIA=1`: openEMS diverges on it, see ELECTRICAL.md). There is no Ubuntu package or Linux wheel: CI builds openEMS-Project from source at a pinned commit (`update_openEMS.sh --python --disable-GUI`, cached), about 10 minutes uncached |
| Fab online DFM checkers | manual, per release, on the demo boards for each supported fab profile |

### Comparison methods

- **Gerbers:** rasterize both at high DPI, XOR, then fail if the differing area exceeds a tolerance. Also compare
  by layer and drill hit by hit (see "Cross-checks" below for the methods and tolerances).
- **DRC:** compare violation sets by (type, objects involved, measured value and location within tolerance).
  Known semantic differences go in a reviewed allowlist with a reason for each entry.
- **Routing:** metric comparison only (completion %, vias, length, time), with regression thresholds.
- **Schematic** (`tests/kicad_oracle.rs`, from M3): the ATtiny85 board is exported with `schematic.export`, then
  `kicad-cli sch erc` must report no errors (allowlisted warnings: `lib_symbol_issues` and
  `footprint_link_issues`, because the embedded `cadlab`/`cadlab_power` libraries are not in KiCad's library
  tables), and `kicad-cli sch export netlist` must give every cadlab net exactly the same (ref, pin) nodes,
  under the same name (KiCad prefixes local labels with the sheet path `/`). A variant with rotated box symbols
  checks the netlist only. Netlist import (M7) closes the loop: KiCad's netlist of the exported schematic,
  read back with `circuit.import` into an empty project, must give the original nets, designators, values and
  pin names and types (ATtiny85 and STM32 boards); `tests/netlist_import.rs` covers cadlab's own netlist
  export → import without KiCad. Tested with KiCad 10.0.6. Example:
  `CADLAB_ORACLES=1 CADLAB_ORACLE_KICAD_CLI=/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli cargo test --test kicad_oracle`.

### KiCad board oracle (`tests/kicad_pcb_oracle.rs`)

`board.export_kicad` writes `.kicad_pcb` (KiCad 8 format, loaded by KiCad 8–10), `.kicad_pro` (board design
rules, net classes with exact-name patterns) and `.kicad_dru` (each net class track width as a minimum width rule,
since KiCad does not check net class widths by itself). `kicad-cli` then checks:

- **DRC, clean board:** the LDO board, fully routed with tracks, vias and a bottom GND pour, has no KiCad DRC
  item (`--severity-all --refill-zones`, no schematic parity) except the allowlist: `lib_footprint_issues`
  (footprints are embedded; there is no `cadlab` KiCad library to configure, D7).
- **Zone fills:** the LDO board with its GND pads joined only by the bottom pour: KiCad's DRC without a
  refill finds no unconnected item, since the export carries cadlab's fill (removing the `filled_polygon`s
  brings back two).
- **DRC, deliberate violations:** KiCad reports `clearance`, `track_width` (net class rule),
  `copper_edge_clearance` and `unconnected_items` for the corresponding defects.
- **Pad placement:** KiCad's IPC-D-356 export (relative to the auxiliary origin, which the export puts at cadlab's
  origin) matches `board::placed_pads` in position, size and angle, for rotated and bottom-side parts.
- **Round trip:** `pcb upgrade --force` re-saves a board using every exported feature without losing items.
- **Gerbers and drill:** KiCad's outputs are kept in `$CARGO_TARGET_TMPDIR/kicad_pcb_oracle/` for the raster
  comparison with cadlab's Gerbers.

KiCad report coordinates are converted back with `kicad_pcb::Frame::from_kicad`.

### KiCad board import (`tests/kicad_pcb_import.rs`)

`board.import_kicad` (D32) is checked by round trips through the exporter, without KiCad: every cross-check
board (below), the small four-layer synthetic board and the STM32 board are exported and imported back

- into the same project with its board cleared (circuit matched): the board model must be equal (object IDs
  aside), the library unchanged, DRC findings equal (code, severity, location), every exported UUID mapped;
- into an empty project (circuit built from the board): the same pads on the same nets, the same DRC findings,
  the same effective rules and net class values.

With `CADLAB_ORACLES=1`, the exports are re-saved by `kicad-cli pcb upgrade` (KiCad's current format: nets by
name, items reordered, lines reversed) and must import to the same board (up to item order) and DRC. A test in
`tests/drc_crosscheck.rs` compares KiCad's DRC of each export with cadlab's DRC of the board imported from it,
with the same matching and allowlist as the DRC cross-check below. `CADLAB_KICAD_PCB_FIXTURES=<dir>` imports
every `.kicad_pcb` under a directory (ad-hoc boards; nothing is downloaded by the test) and, with oracles, has
KiCad load the re-exported result; the pinned open-source corpus below is the systematic version. Unit tests
in `src/kicad_import/tests.rs` cover the older syntax (net tables, `fp_text reference`), custom pads (polygon
outlines, rings), trapezoid and chamfered pads and drill offsets (each next to a custom pad of the same outline, on both sides, exported back), slots, paste apertures, bottom footprints, mounting holes, board-only
stitching vias and hole patterns, logos, keep-outs with wildcard layers and with holes, circle courtyards, text
variables and every unsupported-item diagnostic.

### KiCad library import (`tests/kicad_lib_import.rs`)

`footprint.import_kicad`, `part.import_kicad_sym` and `lib.import_kicad` (D41) run on hand-written fixtures in
`tests/fixtures/kicad/` (cadlab's own files, see its README: KiCad 6 and 8 syntax, every pad kind, custom pad,
slot, trapezoid, circle courtyard, drawings on unsupported layers, a derived symbol, a dual op-amp with a
supply unit and a De Morgan body style, alternate pin functions, stacked and hidden pins, a power symbol,
supplier fields). Round trip without KiCad: the footprints of the placed ATtiny85 board are cut out of
`board.export_kicad` into a `.pretty`, its library symbols out of `schematic.export` into a `.kicad_sym`, and
importing both must give the same land patterns and the same parts (symbol, category, footprint, value,
datasheet, description). With `CADLAB_ORACLES=1`, `kicad-cli sym upgrade` and `fp upgrade` must read the
fixtures and the round-trip libraries, importing KiCad's rewrite (KiCad 10 syntax, items reordered) must give
the same footprints (as land patterns) and parts, and `kicad-cli sym export svg` must draw every fixture
symbol.

### Cross-checks: KiCad DRC vs cadlab DRC, KiCad Gerbers vs ours

Both run on the boards of `tests/crosscheck/mod.rs`: the LDO board fully routed with a bottom GND pour
(`clean`), the same with a QFN at 45° (paste windows), a bottom-side THT header whose pin 1 joins the pour
through a thermal relief, 0402s at 30° and 135° (bottom) and an arc track (`features`), board and footprint mask and paste
margins, a slotted pad, paste-in-hole, a solid pad zone connection and a pad keeping a 0.3 mm local
clearance a track violates (`local_settings`, D40), and one board per deliberate violation: clearance track/track, track/pad, pad/via, short, net class width, minimum width, via
annular ring, via drill, pad drill and annular ring, hole-to-hole, copper-to-edge, courtyard overlap (and a 9.9 µm
overlap neither tool reports, `courtyard_touch`, D56), silk over
pad, silk 0.08 mm from a pad (`silk_near_pad`), unrouted, keep-out (tracks), keep-out (footprints), and a netless track touching a net. Outputs are kept in
`$CARGO_TARGET_TMPDIR/{drc,gerber}_crosscheck/<board>/` for inspection.

**DRC** (`tests/drc_crosscheck.rs`): `drc::check` against `kicad-cli pcb drc --format json --severity-all
--refill-zones` on the export. KiCad types map to cadlab codes in one table (`TYPE_MAP`: `clearance`,
`shorting_items`/`tracks_crossing` → `drc.short`, `track_width`, `annular_width`, `drill_out_of_range`,
`hole_to_hole`, `hole_clearance`, `copper_edge_clearance`, `courtyards_overlap`, `items_not_allowed` → `drc.keepout`,
`silk_over_copper`, `unconnected_items` → `drc.unrouted`). Findings are matched one to one by:

- rule (mapped code; severities may differ: cadlab's class width check is a warning, KiCad's `hole_to_hole` is);
- items: KiCad's item UUIDs map back to cadlab objects through the exporter's table (`KicadExport::uuids`), and
  KiCad's items (board edges aside) must be among the cadlab finding's subjects;
- measured value: KiCad's "actual X mm" and cadlab's distance/size agree within 2.5 µm (cadlab approximates arcs
  outward by ≤ 1 µm per shape, KiCad prints 0.1 µm);
- location: KiCad's report has no marker position, only item anchors; each anchor, converted with
  `Frame::from_kicad`, must be within 1 nm of the cadlab object (pad center, track start, via, footprint origin).

Every unmatched finding needs an allowlist entry with a reason, and every entry must be used:

| Side | Rule | Reason |
|---|---|---|
| KiCad | `lib_footprint_issues` | footprints are embedded; no `cadlab` KiCad library exists (D7) |
| KiCad | `track_dangling`, `via_dangling` | cadlab has no dangling-end rule; connectivity is `drc.unrouted` between pads and vias (a via joined to its net through a pour only is connected) |
| KiCad | `clearance` between pads of one footprint | cadlab does not check pads of one footprint against each other (their spacing is the footprint's); KiCad does (QFN pins 0.16 mm from the exposed pad) |
| KiCad | `silk_over_copper` on a Reference field | reference designators are generated by the legend writer and clipped at mask openings there; `drc.silk_over_pad` checks footprint and board silkscreen |
| KiCad | `silk_overlap` | cadlab has no silkscreen-to-silkscreen rule |
| both | netless track touching a net (`netless_track` only) | cadlab reports `drc.short` (copper must say which net it carries); KiCad gives the track the net it touches on load, then applies that net's class width |
| cadlab | `drc.silk_over_pad` near a pad (`silk_near_pad` only) | cadlab keeps silkscreen `silk_to_pad` (exported as `min_silk_clearance`) from pad copper, a fab limit; KiCad's `silk_over_copper` only reports silkscreen overlapping a mask opening (its silk clearance applies between silkscreen items), so a line 0.08 mm from C2.1 passes there |
| cadlab | `drc.clearance` U1.5 / GND track (`short` only, KiCad ≥ 10.0.7) | KiCad 10.0.7 no longer reports the GND track's 0.156 mm gap to pad U1.5 (3V3) once the same track is reported crossing a 3V3 track; 10.0.6 reported it on the identical board (actual 0.1562 mm), and the gap is a real violation. The entry is only active with `kicad-cli version` ≥ 10.0.7, so it is used on either version |

**Gerbers** (`tests/gerber_crosscheck.rs`): cadlab's `fabout` Gerbers and XNC drill files against `kicad-cli pcb
export gerbers --use-drill-file-origin --check-zones` and `pcb export drill --drill-origin plot
--excellon-separate-th` (both relative to the auxiliary origin, which the export puts at cadlab's origin, so the
coordinates are cadlab's). gerbv renders each layer of both at 1000 dpi (25.4 µm/px) over the same window (the
outline's bounding box plus 1 mm, `--origin`/`--window_inch`), and the images are XORed. A differing pixel counts
only when both images are uniform within 1 px around it (rasterization and arc-approximation jitter is not a
difference). Per layer:

- copper (F.Cu, B.Cu with KiCad's own zone refill), mask, paste: at most 0.01 mm² of differences (about 15 px)
  and total areas within 0.5 % + 0.01 mm² (catches uniform growth below a pixel, e.g. a mask expansion
  mismatch). KiCad refills the pours with its own algorithm; with the export writing cadlab's effective zone
  settings, the fills agree to the pixel on these boards.
- Edge.Cuts: same tolerance; cadlab's profile aperture is 0.1 mm and the exported Edge.Cuts lines 0.05 mm, a
  25 µm difference per side absorbed by the 1 px tolerance, so the outlines' center lines must coincide.
- silkscreen: compared outside mask openings (cadlab clips the legend there; KiCad does not unless
  `--subtract-soldermask`, whose clear-polarity output gerbv's PNG export does not render faithfully) and outside
  reference designators (different stroke fonts), which only have to be inked by both at the same place.
- drill: the same hits with the same plating, hole function and diameter; positions within 1 µm (KiCad writes
  three decimals, cadlab four).

gerbv warns that a Gerber file without apertures "is most likely RS-274D"; both tools write empty layers that
way, so that warning is accepted for such files only.

Running locally (KiCad 10.0.6 or later and gerbv from Homebrew on macOS; CI uses the KiCad 10.0 PPA, 10.0.7 since 2026-10-14):
`CADLAB_ORACLES=1 CADLAB_ORACLE_KICAD_CLI=/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli cargo test
--test drc_crosscheck --test gerber_crosscheck -- --nocapture` (the log lists every matched pair and the
per-layer areas).

### Open-source corpus (`tests/corpus.rs`)

Twelve open-source KiCad projects are pinned in `tests/corpus/projects.toml` (repository, exact commit, sparse
paths, board and root schematic, license, notes, known differences; selection and pinning policy in DECISIONS
D35). Nothing is vendored: `scripts/fetch-corpus.sh [dir]` fetches each project as a shallow, blobless, sparse
checkout of exactly its commit (verifying `HEAD` against the pin) into `target/corpus` by default, and leaves a
project alone when it is already at the pin. The test reads `CADLAB_CORPUS_DIR`, never downloads, and skips
without it.

| Project | License | Layers, KiCad | Covers |
|---|---|---|---|
| `nrfmicro` (joric/nrfmicro) | Unlicense | 2, 6 | small SMD module carrier, solder-jumper custom pads, USB-C slots |
| `buspirate-flash-sop` (BusPirate5-hardware) | MIT | 2, 7 | small SMD/THT adapter, a pour |
| `buspirate-rs232` (BusPirate5-hardware) | MIT | 2, 8 | KiCad 8 format; schematic and board out of sync |
| `buspirate5-rev10` (BusPirate5-hardware) | MIT | 4, 7 | RP2040 main board, hierarchy, dense 0402, mask/paste margins |
| `lumenpnp-ringlight` (opulo-inc/lumenpnp) | CERN-OHL-W-2.0 | 2, 6 | round board with a cutout, LEDs on an arc |
| `lumenpnp-mobo` (opulo-inc/lumenpnp) | CERN-OHL-W-2.0 | 4, 8 | STM32 motherboard, 80+ zones, net tie, pads on the other side |
| `sweep-v2.2` (davidphilipbarr/Sweep) | SHL-2.1 | 2, 6 | reversible keyboard, no schematic (circuit built from the board), mouse bites |
| `corne-cherry` (foostan/crkbd) | MIT | 2, 7 | reversible keyboard, 900+ teardrop zones, both sides |
| `glasgow-revC3` (GlasgowEmbedded/glasgow) | 0BSD | 4, 7 | iCE40 BGA, keep-out frame with a hole, fiducials outside the schematic |
| `glasgow-revD1` (GlasgowEmbedded/glasgow) | 0BSD | 6, 10 | KiCad 10 format, BGA FPGA and RAM, ring-shaped custom pads, net ties |
| `cynthion` (greatscottgadgets/cynthion-hardware) | CERN-OHL-P-2.0 | 4, 7 | ECP5 BGA, Kelvin sense resistors (net ties), text variables, custom rules |
| `tinytapeout-demo` (TinyTapeout/tt-demo-pcb) | Apache-2.0 | 4, 9 | RP2350, 160 stitching-via footprints, geometric custom rules |

Per project, on a copy of the checkout:

1. **Netlist:** `kicad-cli sch export netlist` of the root schematic, read by `circuit.import` (D13, D27).
2. **Board:** `board.import_kicad` with the `.kicad_pro` / `.kicad_dru`, cadlab's origin on KiCad's auxiliary
   origin (so cadlab coordinates are the ones KiCad plots); the export must import back with the same
   placements and copper; the imported project is saved for inspection.
3. **DRC of the original:** cadlab's DRC against `kicad-cli pcb drc --schematic-parity` of the original board,
   with the cross-check matching (`tests/common/drc_compare.rs`), items mapped through the importer's UUID
   table; KiCad's `missing_footprint` parity findings pair with `drc.unplaced`.
4. **Re-export:** `board.export_kicad` of the import; KiCad's DRC of it against cadlab's, through the
   exporter's UUID table.
5. **Gerbers and drill:** cadlab's outputs against KiCad's of the original, rendered by gerbv and XORed
   (`tests/common/gerber_compare.rs`). Copper is compared twice: without pours (KiCad's fills stripped from a
   copy of the board, cadlab's zones removed), which must agree, and with each tool's own refill
   (informative: the fill algorithms differ). Mask, paste and Edge.Cuts are compared as in the cross-check;
   silkscreen is informative (fonts, generated reference designators). Drill hits must match in plating,
   diameter and position, slots as slots (KiCad writes `G85` slots, cadlab routes them; a KiCad slot matched only by a round
   hole at its middle would be counted apart); hole functions are not compared (KiCad's component drill for a stitching-via
   footprint is cadlab's via drill).
6. **Re-route** (`CADLAB_CORPUS_ROUTE=1`, budget `CADLAB_CORPUS_ROUTE_BUDGET` seconds, default 60): tracks and
   vias ripped up and routed again by cadlab's router; completion is reported, never a failure. Without KiCad,
   `cargo run --release --example route_bench -- --corpus [names]` does the same re-route on the boards alone
   (circuit built from the board) and prints completion, vias, length, time and DRC errors per board with the
   failures grouped by reason (`CADLAB_BENCH_ROUTER`, `CADLAB_BENCH_BUDGET_MS`, `CADLAB_BENCH_VERBOSE`,
   `CADLAB_ROUTE_RENDER`; docs/ROUTER.md, "Benchmarks").

Allowances, each with its reason, on top of the cross-check's (`drc_compare::ALLOW`):

- rules only one tool has (`ONE_SIDED_RULES` in `tests/corpus.rs`): KiCad's `silk_edge_clearance`,
  `text_height`, `text_thickness`, `solder_mask_bridge`, `footprint_type_mismatch`, `starved_thermal`,
  `copper_sliver`, `nonmirrored_text_on_back_layer`, `npth_inside_courtyard`, `pth_inside_courtyard`;
  cadlab's `drc.footprint_outside` (KiCad's `hole_clearance` pairs with `drc.hole_clearance` since D56);
- KiCad `silk_over_copper` on board texts (cadlab checks silkscreen drawings, not texts);
- original board only: `drc.track_width_class` (KiCad never checks net class widths; the export adds a
  `.kicad_dru` rule, so the re-export does); cadlab findings of rules the project sets to `ignore` in its
  `.kicad_pro`; courtyard overlaps involving a footprint whose courtyard the import generated;
- re-export only: `drc.unplaced` (no schematic to check parity against);
- report granularity: KiCad reports silkscreen over copper per silkscreen segment, so extra KiCad findings on
  a pair cadlab reports are the same defect; an open connection names one item of each island, either
  tool's choice, so once exact matches are taken a KiCad `unconnected_items` and a cadlab `drc.unrouted`
  sharing an item are the same defect, and so are two that join the same two islands of cadlab's
  connectivity through other items (a zone and a pad in KiCad, two pads in cadlab: D60); KiCad's report stops at 199 findings of most types, so cadlab
  findings of a type KiCad reported exactly 199 times are unverifiable and counted apart;
- board-specific `known` entries in the manifest: side, rule, an exact maximum count, the comparison it
  applies to, and the reason; `gerber_known` entries for layers whose differences are explained. A known
  difference that no longer shows up fails the test too, so the manifest follows fixes.

The summary table (stderr and `$CARGO_TARGET_TMPDIR/corpus/summary.txt`) gives per project the copper layers,
footprints, tracks, netlist components, import diagnostics, items not imported, KiCad/cadlab/matched DRC
findings for the original and the re-export, unexplained differences, routing completion, time and status;
`report.json` next to it has the details (netlist resolution, import counts and diagnostics by code, DRC
outcomes by rule, per-layer Gerber areas and differences, drill counts, routing statistics). Every
intermediate file is kept under `$CARGO_TARGET_TMPDIR/corpus/<project>/` (copy of the checkout, netlist, both
DRC reports, cadlab's DRC as JSON, the imported project, the re-export, Gerbers and renders). CI runs it in the
`corpus` job (KiCad 10 from the PPA, gerbv, release build, routing with a 30 s budget), caching the corpus
by the manifest's hash; the job is informative (`continue-on-error`) like the oracle job. Locally (about four
minutes, six with routing):

```sh
scripts/fetch-corpus.sh
CADLAB_CORPUS_DIR=$PWD/target/corpus CADLAB_ORACLES=1 \
  CADLAB_ORACLE_KICAD_CLI=/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli \
  cargo test --release --test corpus -- --nocapture   # CADLAB_CORPUS_ONLY=name,name for a subset
```

Results with KiCad 10.0.7 at the pins (re-measured 2026-10-15, D56; 10.0.6 gave the same corpus results
before D44): every board imports; the board-level comparison has no unexplained difference; two boards
(`buspirate-flash-sop`, and `lumenpnp-ringlight` apart from one capacitor's Gerber layers) match without any
DRC known entry. Routing everything again completes 61-100 % within
30 s per board (`glasgow-revD1` 61 %, `tinytapeout-demo` 79 %: the router's weakest cases).

**Fixed from the corpus** (each now covered by unit or round-trip tests): zone fills and their DRC disagreed
when a zone's own clearance was below the net clearance (fills now keep the larger); bottom-side pads were
clockwise rings, so zone keep-away unions cancelled where they overlapped a track (fills reached into other
nets); KiCad custom pads were bounding rectangles (now polygon pads, through DRC, Gerber outline macros,
IPC-2581 contours, Specctra and the KiCad export); zones and keep-outs with several outlines kept only the
first (a keep-out frame became a solid keep-out); circle and multi-shape courtyards were exported twice
(KiCad's `malformed_courtyard`); board-only stitching-via and hole footprints were dropped; KiCad's
`min_silk_clearance` was ignored; text variables stayed unresolved; touching courtyards, keep-outs and
silkscreen counted as overlapping through the outward arc approximation (overlaps must now be wider than the
2 µm tolerance); the KiCad export floored net classes with a smaller clearance at the board clearance; mask and
paste margins, local clearances, zone connection overrides, net ties and paste on through-hole pads were
dropped silently (first reported, then modeled, D40); local pad and footprint settings, net ties, pads on the
back of their footprint, per-side and missing mask openings, slots, paste-in-hole, copper, mask and paste
drawings in footprints, copper drawings and texts on the board, filled board polygons, mask-only and
non-rectangular paste-only apertures, the board's mask expansion, minimum web and paste margins, and
`.kicad_dru` rules with geometric conditions are now modeled and exported back (D40): with them, the
known differences fell from 46 DRC entries (812 findings at most) and 48 Gerber layers to 40 entries
(272 findings) and 23 layers (`tests/corpus/projects.toml`; local clearances alone removed 330 neck-down clearance findings on
`tinytapeout-demo`, net ties the Kelvin resistor shorts and opens on `cynthion`). Re-measured after D44–D48
with KiCad 10.0.7 (D51), with the silkscreen minimum measured to copper again (D46 had added it to the grown
mask opening), thermal spokes trimmed at other items' clearances instead of dropped (D46's diagonal spokes
had disconnected squeezed pads), and the import's UUID table covering KiCad 6/7 pad `tstamp`s and the
drawings of pad-less footprints: 32 entries (320 findings at most, 118 of them one board's mask expansion,
see below) and 23 Gerber layers; the pour-connectivity entries went from 6 boards (30 findings) to 3 (11).
Every residual D51 left "not analyzed" was then traced (D56): clearance gaps measured on exact outlines,
courtyard overlaps under KiCad's 10 µm, courtyards in pieces and on the other side, rule areas sharing a
name, renamed designators, tented-only footprint vias, the copper-to-hole clearance in fills, the DRC and
the router, and open connections paired by a shared end brought the manifest to 25 entries (215 findings
at most, 118 of them corne-cherry's mask expansion) and 20 Gerber layers; pour connectivity differs on 2
boards (four GND pads on `buspirate5-rev10`, one on `sweep-v2.2`). Both were then traced (D60):
`buspirate5-rev10`'s were a fill gap (KiCad's fill keeps spokes running from pad to pad along a row of
same-net resistor-array and 0402 pads, which cadlab dropped for not reaching the pour), `sweep-v2.2`'s none
(both tools leave the two keyboard halves' GND apart, as only the cable joins them, and name other ends of
the two islands); with copper rings around non-plated holes modeled (the power switches' pegs), the manifest
has 23 entries (209 findings at most) and 18 Gerber layers, and no pour-connectivity entry.

**Known issues** (recorded per board in the manifest; candidates for later work):

- logos with a schematic symbol (unplaced), unannotated designators (`REF**`) and board footprints missing
  from the schematic (fiducials, placeholders) are not imported (annotated designators cadlab cannot use,
  `POWER SW`, are renamed when the circuit comes from the board, D56);
- per-layer thermal and zone settings of pad stacks take the pad's (D48);
- footprint drawings that are neither copper, mask, paste nor courtyard on the other side of the footprint
  (silkscreen, fab), and the drawings of footprints that become board holes, are not kept;
- custom DRC rules cadlab has no counterpart for (`hasComponentClass`, terms relating both items such as
  `A.Net == B.Net`, disallowed `text`/`hole`/`footprint`, track widths conditioned on `B`) and DRC
  exclusions stored in the project (layer-marker texts outside the outline, overlapping PMOD headers);
  `disallow`, conditions on `B` (a second scope, clearances) and rule severities are modeled since D46;
- texts on copper layers are drawn in cadlab's stroke font (Hershey simplex, public domain), whose glyphs
  differ from KiCad's own font (Gerber copper layers, copper-to-edge near the edge); KiCad's glyph data
  exists only in its sources, so it is not matched (D56);
- silkscreen over solder mask openings (D46, D51): KiCad's check grows pads by pad and footprint mask
  margins but not by the board-level expansion (`pad_to_mask_clearance`), which the fab applies and cadlab
  checks (`corne-cherry` 0.2 mm: 118 findings on the re-export; `cynthion` 0.05 mm); KiCad also does not
  report a logo's silkscreen over its own mask drawings (`sweep-v2.2`);
- net-tie shorts of sense tracks crossing a tied pad are reported by KiCad unevenly (not paired);
- DRC clearance deficits between KiCad's 0.5 µm tolerance and cadlab's 2 µm are KiCad's alone: the router
  keeps clearances to 1 µm, and tightening it changes routes beyond the benchmark's limits (D60).

### freerouting oracle (`tests/specctra.rs`)

Two boards are exported with `export.dsn`, routed by freerouting headless (`-mp 20 -mt 1`, a separate `java`
process) and imported with `route.import_ses`; cadlab DRC must then report no error, unrouted connections
included. This checks the DSN geometry end to end (a wrong back-side transform shows up as shorts and opens) and
the exact unit conversion of the session. `CADLAB_ORACLE_FREEROUTING` points at the jar (run with `java -jar`)
or a launcher; `CADLAB_SPECCTRA_KEEP=<dir>` keeps the DSN, SES and a render. Tested with freerouting 2.1.0, which
sometimes drops wiring from its session while reporting a complete route; those runs are retried (details in
ROUTER.md). Not in the CI oracle job, which installs no Java. Example:
`CADLAB_ORACLES=1 CADLAB_ORACLE_FREEROUTING=$HOME/freerouting.jar cargo test --test specctra -- --nocapture`.

### Exchange outputs (`tests/exchange.rs`)

IPC-2581, STEP, IDF and IDX have golden files (`tests/golden/exchange/`) and structural checks that parse the
files back: a minimal XML reader (references to dictionaries and layers resolve, counts match the board; for IDX
every `id` is unique, every IDREF resolves, and holes, keep-out bounds, body heights and transforms match the
board), a Part 21 reader (every `#ref` defined, every B-rep shell closed: each edge used once in each direction)
and an IDF section reader. Three optional oracles skip when the tool or schema is missing even with
`CADLAB_ORACLES=1`:
`CADLAB_ORACLE_FREECAD` (`freecadcmd`, e.g. `/Applications/FreeCAD.app/Contents/Resources/bin/freecadcmd`) must
read the STEP file as valid closed solids, the board volume matching the outline minus cutouts and holes, and
every generated body solid with a positive volume; `CADLAB_IPC2581_XSD` (a local copy of IPC's `IPC-2581C.xsd`, not shipped) validates the XML with
`xmllint --schema`; `CADLAB_IDX_XSD` (the directory of the IDX schema files from prostep ivip's free
`PSI5_IDXv4.5_release.zip`, not shipped) validates the IDX baseline, change and response messages the same way (verified with the V4.5 schema,
2026-10-04, change and response messages 2026-10-14). `CADLAB_STEP_KEEP=<dir>` keeps the STEP file of
`step_generated_bodies` (one part per package family) for a viewer. No ODB++ or IDX reader is available as an oracle: KiCad exports ODB++ but not IDX, and cadlab
writes no ODB++ (DECISIONS D37). Example:
`CADLAB_ORACLES=1 CADLAB_ORACLE_FREECAD=/Applications/FreeCAD.app/Contents/Resources/bin/freecadcmd cargo test --test exchange`.

3D models (`tests/models3d.rs`, D36) use small boxes the tests write themselves as STL, OBJ and glTF (no
third-party models). The 3D view is checked with pixel probes where the model stands (above and under the board)
and identical bytes for identical input; the STEP faceted B-rep is parsed back (closed `POLY_LOOP` shells,
welded corners at the expected coordinates), and the same FreeCAD oracle must read the model body as a valid
closed solid of the box's volume (`--test models3d`).

### Oracle availability

- Oracle tests are behind an env flag (`CADLAB_ORACLES=1`). Without it they skip with a message; with it, a
  missing tool fails the test, so CI never skips silently. Tool paths can be overridden with
  `CADLAB_ORACLE_KICAD_CLI`, `CADLAB_ORACLE_FREEROUTING`, ... (`tests/common/mod.rs`).
- Golden files live next to the tests (`tests/golden/`); `CADLAB_BLESS=1 cargo test` rewrites them. Snapshot
  tests use `insta` (`cargo insta review`).
- CI runs them in the `oracles` job (`.github/workflows/ci.yml`): Ubuntu 24.04, KiCad 10 from
  `ppa:kicad/kicad-10.0-releases` (installed without the libraries), gerbv from Ubuntu, tests under `xvfb-run`,
  outputs uploaded as an artifact. The job is `continue-on-error` while the PPA install proves stable; the PPA
  follows 10.0.x patch releases, so results are reproducible per KiCad minor version only. Target: a container
  with pinned versions of each oracle.

## Benchmarks in CI

The `bench` job (`.github/workflows/ci.yml`, blocking, D43) runs two `#[ignore]`d tests in release with
`RAYON_NUM_THREADS=1`, uploads their reports (`target/tmp/bench/{route,perf}.{json,md}`, the Markdown also in the
job summary) and fails on:

- **Router regressions** (`tests/route_bench.rs`, `route_benchmark_regression`): the generated boards of the
  router benchmark (all but the budget-bound `big-4l`) against `tests/golden/bench/route.json`: any DRC error, an
  exhausted budget, a completion drop, more than max(2, 5 %) extra vias or 2 % extra track length. The routes are
  deterministic, so the baseline is exact; the tolerances only keep small trade-offs from failing CI. The default
  `cargo test` checks the baseline's board list and routes the smallest board (exact match).
- **Large slowdowns** (`tests/perf.rs`, `big_board_timings`, and the routing time of each board above): every
  heavy step on the synthetic board (load, save, zone fill, connectivity, ratsnest, DRC, render, Gerber and KiCad
  export, schematic) against `tests/golden/bench/perf.json`. CI machines vary, so a step fails only when it is
  more than 250 ms slower than its baseline and either 3× slower than the run's median ratio to the baseline (the
  machine's speed cancels out) or 8× slower outright (`tests/common/bench.rs`). Debug builds report, never fail.

Locally: `RAYON_NUM_THREADS=1 cargo test --release --test route_bench --test perf -- --ignored --nocapture
route_benchmark_regression big_board_timings`; add `CADLAB_BLESS=1` to rewrite both baselines after an intended
change (commit them with it; the router baseline's `ms` and the timings are machine-dependent and only feed the
timing rule).

## Test corpus

- **Generated designs:** boards built by cadlab scripts (`tests/corpus/*.jsonl` command batches). These are fully
  ours and cover features systematically.
- **Open-source hardware projects:** fetched at CI time from their repositories (pinned commit), used as KiCad import
  and oracle inputs. Not vendored into the repo, and their licenses are recorded in the corpus manifest
  (`tests/corpus/projects.toml`, `scripts/fetch-corpus.sh`; see "Open-source corpus" above, D35).
- **Router benchmarks:** DSN files from the generated designs plus fetched projects; synthetic stress cases.

## Agent sessions

The roadmap's "agent ergonomics" item: an agent designs a board from a prompt using only the built `cadlab` binary
(`--help`, `describe`, `--json`, error messages; no source), keeps a log of every place it got stuck, the
friction points are fixed with a test each, and the final session is kept as a regression test.

### Session 1 (2026-10-14): ATmega328P, USB-C power, JLCPCB bundle

Prompt: *"Design a 2-layer 50x40 mm board: ATmega328P or STM32 MCU, USB-C power input with ESD protection, 3.3 V
LDO, crystal, SWD/ISP header, two status LEDs, I2C header with pull-ups. Generic parts are fine where no supplier
is configured. Place, autoroute, pass ERC/lint and DRC, render schematic and board, and export a JLCPCB fab
bundle."*

Result: 24 components (ATmega328P-AU TQFP-32, AP2112K-3.3 LDO, SMF5.0A TVS on VBUS, 5.1 kΩ CC pull-downs,
8 MHz 3225 crystal with 22 pF loads, 2x3 ISP header, 1x4 I2C header with 4.7 kΩ pull-ups, two LEDs), auto-placed
around a locked USB-C connector, 100 % routed in ~0.1 s, ERC/lint/DRC clean, JLCPCB bundle exported. Replayed by
`tests/agent_session.rs` (fixtures in `tests/fixtures/agent_session/`: the MCU pin list and a hand-written
USB-C land pattern); each fix below has a test in `tests/agent_ergonomics.rs`.

Friction log, and what was done:

| # | Where the agent got stuck | Fix |
|---|---|---|
| 1 | `cadlab --help` listed groups as "part commands", "net commands": no clue where `erc` lives | One-line group summaries (`commands::GROUP_SUMMARIES`), used by CLI help and MCP tool descriptions |
| 2 | `cadlab erc` (as in INTERFACES.md) failed, and clap suggested `drc`; `cadlab drc` printed help with exit 2 | Top-level `cadlab erc` / `cadlab lint` (aliases of `circuit erc` / `circuit lint`); `cadlab drc` alone runs `drc run` |
| 3 | `--pins @pins.json` rejected: long pin lists had to be pasted inline | Every JSON-valued flag (and `cadlab call`) accepts `@file` |
| 4 | `place set --at "25mm,35mm"` → "expected JSON: trailing characters"; points had to be written `'["25mm","35mm"]'` | Fixed-size tuples (points) take `x,y` on the CLI (`--at <X,Y>`); JSON still works |
| 5 | Unknown package (`USB-C`, `HC-49`): no hint; "or give explicit dimensions" did not say how; `TQFP-N` and `PinHeader 2xN` worked but were not listed | `footprint.unknown_package` hint names `spec` / `package_spec` and `footprint.import_kicad`; list completed |
| 6 | `part.create` of a connector without `package` succeeded silently, leaving a part that can never be placed | `part.no_footprint` warning with the fix; `part.show` prints `footprint: none` |
| 7 | `part.show` printed `PowerOut`, `(Led)`, while arguments are `power_out`, `led` | Summaries print enum values as typed in arguments (`util::wire_name`), also in `footprint.show` |
| 8 | ERC `erc.output_conflict` on VBUS and GND: the USB-C connector's two VBUS (A9/B9) and GND contacts counted as two drivers | Same-named output pins of one component count as one driver; two parts still conflict |
| 9 | After `board.rules --fab jlcpcb`, 17 `drc.silk_over_pad` warnings on cadlab's own generated footprints (silk 0.15 mm from pads vs a "comfortable" 0.19 mm), with the hint "move the part", which cannot help | `comfortable` keeps silk limits as published (the fab clips silk, a margin buys no yield); silk of a footprint near its own pads gets a hint that fits |
| 10 | `fab.no_suppliers` hint ("configure DigiKey") disagreed with `supplier.none` (catalog import, Mouser, Nexar); `catalog list` said "no providers" without a hint | One shared hint, `supplier::NO_SUPPLIERS_HINT` |
| 11 | `fab.silk_width` hint: "widen the footprint's silk lines in the project library", but no command edits silk widths | Hint names re-importing (`footprint.import_kicad --replace`) or regenerating |
| 12 | `route all --budget-ms 60000`: the only unitless quantity in the CLI; docs said `--budget 60s` | `budget` with a unit (`60s`, `500ms`, `2min`) on every route command; `budget_ms` kept |
| 13 | `render board out/x.png` and `render schematic out/x.svg` take the path positionally, `render footprint/symbol` needed `--path` | Path positional for all render commands |
| 14 | Subcommand help mixed `-p/--json/--dry-run/-q` into each command's options | Global options under their own "Global options" heading |
| 15 | INTERFACES.md examples (`cadlab erc`, `place U2 10mm 15mm`, `board outline rect`, `route --all --budget`, `bom add`, `export fab --fab`, `project new --layers`) did not exist | Examples rewritten to the real commands |

Seen but left as they are (noted for the next session):

- `place.auto` packs everything into a narrow column (here ~18 mm wide on a 50 mm board) rather than using the
  board; fine for routing, but connectors and headers are not pulled to edges unless placed and locked first.
- `drc` before routing lists every unrouted connection (59 errors here); `route status` is the better first look.
- Text `describe` shows nested object fields (`pins: {number, name?, kind?, ...}[]`) without their enum values;
  `describe --json` has them.
- `footprint.import_kicad` names a footprint after its file, not the `(footprint "...")` name inside; the
  not-found error's "did you mean" recovered it at once.
- No `footprint.create` from a pad list: land patterns no generator family covers (USB-C) go through a hand-written
  `.kicad_mod`, which an agent can write from KiCad's published format but which is a detour.

### Session 2 (2026-10-14): session 1's open items

The five items above were fixed (D50), each with a test in `tests/agent_ergonomics.rs`, and the session was
replayed with them: `tests/agent_session.rs` now builds the USB-C land pattern with `footprint create` from a
pad list (`tests/fixtures/agent_session/usb_c_6p_pads.json`) instead of importing the hand-written
`.kicad_mod` (which stays as the fixture of the import-name test), checks that the placement uses the board and
that the check before routing is one summary.

| # | Open item | Fix |
|---|---|---|
| 1 | `place.auto` packed everything into an 18 mm column | Groups spread: rooms sized from the free board area, the largest IC centered first, connectors kept apart along the edges, satellites (a crystal) inside their IC's room, anchors held in their room during improvement; part boxes include silkscreen (a pin-1 dot fell on a neighbour's pad once groups moved); `compact: true` keeps the packed placement. Here: the MCU central, the ISP header at the bottom edge, the I2C header at the right edge, the LDO group by the USB connector |
| 2 | `drc` before routing listed 59 unrouted connections | More than 10 become one `drc.unrouted` with the count per net (`59 connection(s) on 18 net(s) are not routed: GND 22, +3V3 12, ...`), the total in the output's `unrouted`; `--unrouted each` / `summary` choose |
| 3 | Text `describe` hid enum values of nested fields | Listed under the argument: `pins[].kind: input|output|...`, `pins[].side: left|right|top|bottom` |
| 4 | KiCad footprint import named after the file | Named after the footprint inside; `import.name_from_file` (info) when they differ; `--footprint` selects by either name; `--name` renames; symbols' `Footprint` fields (file names) still resolve |
| 5 | No footprint from a pad list | `footprint create <name> --pads @pads.json [--body ...]`: pads with number, at, size, shape, drill/slot, layers, rows by `count` + `pitch`; silkscreen, pin-1 dot, fab outline and courtyard generated |

Result: same 24 components, 100 % routed (59 connections), ERC/lint/DRC clean, JLCPCB bundle exported.

What I would fix next (seen in the replay, not fixed):

- `footprint create` centers the body on the origin, so a connector whose datasheet origin is at its pads
  (the USB-C receptacle) needs its pads shifted by hand, and `place set` then positions the body center. A
  body offset (`body.at`, also for 3D previews and MCAD exports) would remove that arithmetic.
- `place auto` has no say in which edge a connector takes (the agent places and locks it first), and
  indicator LEDs are not pulled to an edge where they would be visible: per-part hints (`--edge J2=bottom`,
  `--edge D2,D3`) would cover both.
- The USB-C shell pads (S1, S2) stay unmapped (`part.unconnected_pad` at `part create`); the hint says
  `pin_map`, but the agent has no shield pin to map them to. A `SHIELD` pin mapped to both pads and tied to
  GND through a net tie or a resistor is the usual answer and could be suggested in the hint.
- Spreading leaves part of a roomy board empty (here the left third): groups follow their connections, which is
  right for routing; a target density per board (`place auto --density`) would let an agent trade
  compactness for room when it wants to. Spreading also costs track: the router benchmark boards (which stay
  on `compact`) route at 100 % either way, but spread they need more length (ATtiny 104 → 136 mm) and vias
  (STM32 2-layer 57 → 84); ordering pins of a connector to face its IC would win some of that back.

### Session 3 (2026-10-15): STM32 USB device, four layers, 90 Ω pair

Session 2's four open items were fixed first (D53, tests in `tests/agent_ergonomics.rs`): `body.at` and
`origin` in `footprint.create`; `place.hint` (per-part edge, persisted) and `place.auto --density`;
`part.unmapped_shield` suggesting the shield pin. Then the session, with the built CLI only.

Prompt: *"4-layer 60x45 mm board: STM32 (LQFP-48) with USB full-speed device on USB-C (D+/D- as a 90 Ω
differential pair with ESD), SPI NOR flash, 3.3 V buck converter from 5 V with its inductor and caps, SWD header,
8 MHz crystal, reset button, two LEDs. Use a JLCPCB 4-layer stackup, solve the USB pair impedance, place, route
(pair first), length-match D+/D-, pass ERC/lint/DRC, render, export JLCPCB bundle and STEP."*

Result: 30 components (STM32F103C8T6, USBLC6-2SC6 flow-through ESD, W25Q32 SOIC-8, SY8089 buck with 2.2 µH
1210 inductor, 16-pin USB-C receptacle from a pad list with an offset body, 2x5 1.27 mm SWD header, tactile
switch from a pad list), JLC04161H-7628 stackup, USB class solved to 0.268 mm / 0.15 mm gap (89.97 Ω), GND and
3V3 planes on the inner layers, the pair routed coupled on F.Cu first (connector → ESD → MCU), everything else
100 % routed, skew tuned to 0.057 mm (limit 0.1 mm), ERC/lint/DRC clean, JLCPCB bundle with the stackup in
`fab-lock.json`, STEP with 30 bodies. Replayed by `tests/agent_session3.rs` (fixtures in
`tests/fixtures/agent_session3/`).

| # | Where the agent got stuck | Fix |
|---|---|---|
| 1 | "Use a JLCPCB 4-layer stackup": nothing knows one; three `board.dielectric` calls from memory, warning about the total thickness after each | `board.fab_stackup jlcpcb JLC04161H-7628` from the profile's `[[process.stackup]]` (JLCPCB's impedance page, three 1.6 mm stackups) |
| 2 | The stackup page's 15.2 µm inner copper then failed `fab.check` (`fab.copper`: offered 17.5 µm) | A published stackup's copper counts as offered |
| 3 | The order needs the stackup named, the bundle did not say which | `fab-lock.json` `process.stackup` (and the export summary) when the board matches one exactly |
| 4 | Adding the SHIELD pin the new warning asked for: `part.create` refused the existing ID, so `part remove` + create | `part.create --replace` (refused when a connected pin would vanish); hint suggests a pin numbered like the pads when they share one number |
| 5 | `erc.power_not_driven` on +3V3: the buck output comes through its inductor | Power propagates through two-pin inductors, ferrite beads and fuses |
| 6 | `lint.missing_decoupling` asked for a capacitor on the buck's switch node | A regulator output driving an inductor is checked behind it |
| 7 | `place auto` put the buck's inductor next to the MCU (largest IC on +3V3) and its input capacitor at the ESD chip | Spread placement: smallest shared power net first, regulator inputs first for a rail's capacitors |
| 8 | The USB-C receptacle at its edge hint, but opening sideways | Connectors that open on one side face their edge (spread placement) |
| 9 | `zone add --outline board` (as the help says) failed: "expected JSON" | Bare words are JSON strings for JSON-valued flags |
| 10 | `route diffpair`: "no coupled path" from the MCU: the D+ pull-up had been placed in the pair's breakout | Passives keep out of differential pair breakouts (spread placement) |
| 11 | `route diffpair` reported the USB-C's two contact rows (D− D+ D− D+) as a failed flow-through link | Interleaved contacts are left to the ordinary router (`route.diffpair_left` info) |
| 12 | `drc.silk_over_pad` on the generated USB-C footprint: the pin-1 dot sat on the shell pad | The dot goes above or below pad 1 when another pad is left of it |
| 13 | `footprint show` hid the body (and its offset) and the holes; `fab show` did not list stackups | Body line and drill/slot per pad; stackups listed |
| 14 | Session-3 test: `place hint J1 --edge left` on three identical headers ended at the top | Improvement no longer swaps parts with different hints |

What I would fix next (seen in the session, not fixed):

- Pair lengths count every branch of a net (the D+ pull-up stub, the receptacle's row link), so the "skew"
  of 6 mm before tuning was mostly stubs, and `route tune` compensated it with 30 bumps on the coupled
  section (coupled 13 mm, uncoupled 31 mm afterwards). Measuring a pair between its ends (stubs excluded,
  reported separately) would keep the coupling.
- The pair is routed only on the layer the impedance was solved for; the ESD array's GND/VBUS pins between
  the flow-through lines still need the router's vias, as noted in ROUTER.md.
- Spread placement follows connectivity: the right third of the board stays empty (the connector, buck and
  ESD gather at the bottom left). `--density 90` spreads rooms but moved the receptacle to the far side and
  lengthened the ratsnest; a target "use the board evenly" is not what the ratsnest wants.
- `board fab-stackup jlcpcb` without a name is an error listing the stackups (`fab show` lists them too); a
  query form (`board fab-stackup jlcpcb --list`) would read better.

Fixed afterwards (D55; the replay runs with them): pair and length-group lengths follow the signal path between
the end components (here J1 → U3 by default), the D+ pull-up and the second contact row are stubs reported by
`diffpair list` (`drc.diffpair_stub` above `--max-stub`, default 5 mm), so the routed pair shows no skew before
tuning and `route tune` adds no bump (coupled 30 of 39 mm, the rest breakouts); spread placement stretches and
relaxes the groups (the SWD header now on the right third, the MCU still central, the pair's ends where their
breakouts were planned); `board fab-stackup jlcpcb` lists the stackups. Tests: `tests/agent_session3.rs`
(tightened: path ends, stubs, at most 2 skew bumps, coupled ≥ 70 %, parts in every third of the board),
`tests/diffpair.rs` (`pair_length_follows_the_signal_path_and_reports_stubs`), `tests/agent_ergonomics.rs`.

D55 also held the pair's end parts still during improvement: with the receptacle a few mm closer to the ESD
array, `route diffpair` failed with "polarity". D61 found two causes: the pair's plan hung the flow-through
array off the receptacle–MCU link on a branch (or joined it from the side that makes the nets cross), and a
moved end part could bring its breakout onto a passive. The array now sits in the signal path, a side that would
cross takes the other way through it, and improvement moves end parts only where their breakouts stay clear of
passives; the hold in improvement is gone (the group moves still keep the ends' arrangement).
`tests/diffpair.rs` (`pair_routes_wherever_the_receptacle_sits`: 21 receptacle positions, 12 of which failed
before) checks it.

### Session 4 (2026-10-15): KiCad migration through the MCP server

This time the agent used only `cadlab mcp` (JSON-RPC over stdio, a small Python client of its own): the
initialize instructions, `tools/list`, `describe`, results and error payloads, no CLI and no source.

Prompt: *"Take an open-source KiCad project (2 layers), import its netlist and board, inspect it (summaries,
DRC, BOM), make a design change (swap a part for a different package, add an LED indicator with its resistor
on a power rail, add a mounting hole, re-pour zones, rip up and reroute the affected nets), pass DRC, and
export fab outputs for two fabs plus a KiCad re-export."*

Project: `lumenpnp-ringlight` from the corpus (round board with a central cutout, eight WS2812B, a level
shifter, two JST connectors on a tab; CERN-OHL-W). Netlist exported with `kicad-cli`, imported, then the
board. Changes: R1 0805 → 0603 (a first attempt, the 74AHCT1G32 from SC-70-5 to SOT-23-5, did not fit the
crowded tab and was undone with `history.undo`), a red power LED with 1 kΩ on +5V placed by `place.auto`, a
2.2 mm mounting hole, R1's nets ripped up and rerouted, the LED routed, zones refilled, 100 % routed. DRC: two
connectors whose courtyards reach 0.03 and 0.15 mm over the tab's edge (as in the original) and the
original's thin and clipped silkscreen waived with reasons, then clean; JLCPCB and PCBWay bundles and the
KiCad re-export written (KiCad's DRC of the re-export after a zone refill: no unconnected pads, no clearance
errors). Replayed by `tests/agent_session4.rs`: `migration_of_a_cadlab_board` always runs (the "KiCad
project" is one cadlab writes first through MCP), `migration_of_the_lumenpnp_ring_light` when
`CADLAB_CORPUS_DIR` is set (netlist from `kicad-cli` with `CADLAB_ORACLES=1`, else the circuit from the board).

MCP friction:

| # | Where the agent got stuck | Fix |
|---|---|---|
| 1 | Hints are CLI syntax: `net.set +5V --driven` became `{"net": "+5V", "driven": true}` (wrong: the field is `nets`, a list) | The server rewrites command spans in hints to `net.set {"nets":["+5V"],"driven":true}` with the CLI's own argument parser; the replay calls hints as written |
| 2 | `history` "alone: list steps" per its description, but the tool said `missing action` | `history` and `drc` run their default action; other tools say which actions exist |
| 3 | Server errors (`missing action`, `describe batch`, bad `steps`) were `{"message"}` without code or hint | Same shape as command errors: kind, code, message, hint (`mcp.missing_action`, `describe.meta_tool`, `command.unknown` with suggestions) |
| 4 | `describe board.rules` returned 16 KB, most of it the output schema, plus a `cli:` line | Output schema only with `output_schema: true`; text names the tool and action and a one-line `returns:` |
| 5 | `drc.run` returned 43 KB (76 identical silkscreen warnings) with no way to narrow it | `severity`, `codes`, `limit`, `offset`; `by_code` counts and a by-code summary line |
| 6 | `{"at": [1, 2]}`: "invalid type: integer `1`, expected a string", without saying which argument | "`at[0]`: expected a string, got 1 (lengths ... are strings with units)" |
| 7 | `describe`/`call` properties had no descriptions; unknown tool names got a bare JSON-RPC error | Descriptions on every property; "did you mean" in the error |

General friction:

| # | Where the agent got stuck | Fix |
|---|---|---|
| 8 | 76 `fab.silk_width` "0mm wide" warnings: the kibuzzard labels (filled silkscreen polygons in pad-less footprints) were imported as zero-width outlines, which print nothing | Filled drawings stay filled polygons (render, Gerber, KiCad export) |
| 9 | Fiducials (symbols without pins) got a phantom pin: `import.pad_missing` ×3, `erc.unconnected` ×3, and "FID1." in messages | Their pin is no-connect; footprints without numbered pads report nothing missing; unnumbered pads print as the designator |
| 10 | 11 identical `import.placeholder_part` warnings (17 KB), claiming footprints "not in the project" after the board import had added them | One warning per part; dropped once the board gave the part its footprint; hint names `board.import_kicad` |
| 11 | `bom.replace` to the 0603 resistor changed nothing on the board: the imported placement kept its own footprint | Placements drop their footprint choice when the part changes (`bom.replace`, `part.set footprint`); `board.footprint_changed_routed` names the nets to rip up |
| 12 | `place.auto` put the new LED on a GND via | Placement keeps clear of existing tracks and vias (with the clearance) |
| 13 | Two connectors over the tab's edge by design: an error with "move it inside", no way to accept it | `drc.waive` / `unwaive` / `waivers` with reasons; the message says by how much the courtyard overhangs |
| 14 | `zone.split`: "split into 3 islands", nowhere to put the stitching via | Areas and locations of the smaller islands |
| 15 | `render` `highlight: ["net:GND"]` refused, though findings print subjects that way (and INTERFACES.md shows it) | `net:` prefixes and pins accepted |
| 16 | MPNs in the netlist's `Part #` fields were ignored (BOM "no MPN yet") | `Part #`-style fields are MPN fields |
| 17 | Stale hint "impedance control comes in M8" on import | Points at `board.dielectric` / `board.fab_stackup` |

What I would fix next (seen in the session, not fixed):

- Netlists need `kicad-cli` (D13 keeps `.kicad_sch` out); an MCP agent without KiCad installed can only build
  the circuit from the board, which loses the schematic's view (here C8 vs the board's C10). A schematic
  netlist reader would close the gap, or the import could say which board footprints the netlist lacks.
- KiCad's DRC exclusions (`.kicad_pro`) are not imported as waivers; the original's own waived findings come
  back as errors on import.
- `place.near` finds no room on a routed board (the free area now excludes tracks) and `place.auto` placed
  the LED's resistor 15 mm from the LED: their shared net does not outweigh both parts' pull to +5V/GND. A
  "keep together" hint (`place.hint --near D9`) would say what the agent meant.
- `route.nets` failed one GND connection that `route.connection --shove` routed; the failure hint suggests a
  smaller clearance instead of shoving.
- `fab.export` text lists every file by absolute path (15 lines of ~180 characters); `circuit.list` shows the
  part's footprint, not the one the placement uses; the BOM calls placeholders "generic"; the KiCad
  re-export has no zone fills (KiCad reports every zone-connected pad unconnected until a refill).
- Supplier part numbers in the netlist (`LCSC` fields) are kept as properties but not offered to
  `fab.substitute` for the JLCPCB BOM.

Session 4 follow-up (D62): the open items above, except the schematic netlist reader, which stays open (D13).
Each has a focused test in `tests/session4_followup.rs`, and the replay uses them where they remove a
workaround:

| Open item | Fix | Replay |
|---|---|---|
| KiCad DRC exclusions not imported | `board.import_kicad` turns the `.kicad_pro` `drc_exclusions` into waivers of the cadlab findings they match (type mapped, items through the UUID table, a finding naming them); the rest reported by cause (`import.drc_exclusion`). On the corpus's Cynthion: 8 of 13 become waivers (courtyard overlaps, copper to edge), 5 have no cadlab check (footprint library mismatch, holes in courtyards). The ring light's project has none | — (the ring light excludes nothing) |
| `place.auto` put the LED's resistor 15 mm from the LED | `place.hint R2 --near D9` keeps a part next to another (target pad of the shared net, pull and tether stronger than any net) | both variants hint `R2` near the LED: under 3 mm |
| `place.near` finds no room on a routed board | courtyards keep clear of tracks and vias first; when nothing is in reach, the body may cover other nets' copper while the pads keep their clearance, reported as `place.over_routing` with the nets | — (focused test) |
| `route.nets` failed a connection `route.connection --shove` routed | `route.nets` gives leftovers `route.connection`'s treatment (push-and-shove, then rip up and reroute, `shove` on by default); failures elsewhere name `route.connection` first | — (focused test: a net walled off on F.Cu) |
| `fab.export` printed 15 absolute paths | the directory once, file names wrapped at ~100 characters; paths relative to the project | summary lines under 120 characters, no absolute path |
| `circuit.list` showed the part's footprint | the placement's (an imported board's) with the part's alongside | `placed_footprint` reads `circuit.list` alone |
| the BOM called placeholders "generic" | `generic` lines, `placeholder` lines and others worded apart | — (focused test) |
| the KiCad re-export had no zone fills | fills written (`filled_polygon`, fractured), `fill yes` | KiCad's DRC of the re-export runs without `--refill-zones`: no unconnected pads (both variants) |
| `LCSC` fields not used | component fields with a supplier's part number fill the fab BOM's part number column and come first among substitutes (basis `field`) | the ring light's JLCPCB BOM carries its LCSC numbers |


### Session 5 (2026-10-22): mixed-signal data logger with blocks, through the MCP server

The agent used `cadlab mcp` only (a small Python client: `tools/list`, `describe`, results and errors).

Prompt: *"A 2-channel 24-bit ADC data logger on four layers: STM32-class MCU, precision ADC with a voltage
reference, an analog front end (one op-amp block instantiated twice), separate AGND and DGND joined at a star
point with split pours, LDOs for the analog and digital rails, USB-C and a microSD card. Parts, circuit, hierarchy,
ERC, placement, routing, pours, DRC, SPICE where it applies, fab outputs."*

Result: 61 components (STM32F411CEU6 QFN-48, ADS1220 TSSOP-16 with a REF5025 2.5 V reference, two OPA376
gain-2 channels as instances CH1/CH2 of block `afe`, ADP150 analog and AP2112K digital LDOs with a ferrite bead
between the rails, USBLC6-2SC6, USB-C and microSD from pad lists, SWD header, 4-pole analog input, a two-pad
net-tie star point between AGND and GND). 70x50 mm, JLC04161H-7628 stackup, USB pair solved to 90 Ω; AGND/GND
split on In1.Cu and B.Cu and +3V3A/+3V3 on In2.Cu at x = 30 mm, analog parts left of it and digital parts right
of it (`place.hint --region`). USB pair routed coupled, everything else 100 % routed in about 2 s (release), DRC
without errors, the ADC's SPI crossings of the split waived with a reason, CH1 exported to SPICE alone with an
ideal op-amp model, JLCPCB bundle, STEP and KiCad export. Replayed by `tests/agent_session5.rs` (fixtures in
`tests/fixtures/agent_session5/`), which checks each fix along the way.

| # | Where the agent got stuck | Fix (D66) |
|---|---|---|
| 1 | `block.create` captures components but leaves them outside any instance: to get CH1 and CH2 the agent removed the originals and instantiated twice | `block.create --instance CH1`: the captured components become the first instance, internal nets renamed `CH1/<net>`; info `block.not_instance` without it |
| 2 | `circuit.list` / `circuit.summary` did not say which component belongs to which instance | `instance` column and filter; summary lists instances with members and ports |
| 3 | `erc.power_not_driven` on AGND although the net-tie part joins it to GND; the hint said to mark AGND `--driven` | ERC follows net ties like ferrite beads; the hint for a ground net names a net tie first |
| 4 | Before wiring, `circuit.erc` returned 37 KB of unconnected pins with no filter (the server instructions say long check results take `codes`/`severity`/`limit`) | `circuit.erc` takes `severity`, `codes`, `limit`, `offset`; reports `by_code`, `listed`, `not_listed` |
| 5 | `place hint J4 --edge left`: the terminal went to the top edge (the analog groups' rooms covered the left edge) | Explicit edge hints take the valid poses closest to their edge first |
| 6 | No way to keep the analog parts on the analog side: `place.auto` put a front-end op-amp over the digital ground | `place.hint --region {from, to}` (block instances accepted as designators) |
| 7 | Nothing checked signals crossing the AGND/GND split | `drc.split_crossing` warning per net and layer, located; waivable |
| 8 | SPICE wrote the net tie as a comment, leaving AGND (and the whole front end) without a DC path to node 0; the agent could not export one channel alone | Net ties written as 1 mΩ; `export.spice --components CH1` |
| 9 | The net tie appeared in the BOM ("no MPN"), the CPL and as a missing STEP body | Net ties left out of BOM, placement files, component layers and STEP; KiCad export marks them excluded |
| 10 | Ground symbols looked the same for AGND and GND on the schematic | Ground symbols print their net's name unless it is `GND` |
| 11 | `place rotate U8` on a routed board left the tracks at the old pads: six DRC errors (shorts) | `place.moved_routed` names the nets and the rip/route commands; `rip: true` rips them |

Seen but left as they are (noted for the next session):

- `part.set` cannot change one pin's kind (REF5025's TRIM/NR declared `input` raised `lint.floating_input`); the
  agent re-sent the whole pin list with `part.create --replace`. A pin patch (`part.set --pins`) would do.
- The first spread placement put the USB ESD array so that the pair would cross between it and the MCU
  (`route.diffpair` polarity failure); the run with regions did not, but nothing guarantees it.
- `describe part.create` is 26 KB, almost all of it the `package_spec` schema.
- `VCAP1` (the MCU's core regulator capacitor, outside any region hint) was routed over the analog ground and
  stays a `drc.split_crossing` warning; hinting the MCU's passives with its region would have avoided it, which
  suggests regions could pass from an IC to its decoupling capacitors.
