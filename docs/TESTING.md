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
| Benchmarks | load, DRC, zone fill, routing, render (`criterion`) | `benches/` |

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
outlines, rings), trapezoid pads, slots, paste apertures, bottom footprints, mounting holes, board-only
stitching vias and hole patterns, logos, keep-outs with wildcard layers and with holes, circle courtyards, text
variables and every unsupported-item diagnostic.

### Cross-checks: KiCad DRC vs cadlab DRC, KiCad Gerbers vs ours

Both run on the boards of `tests/crosscheck/mod.rs`: the LDO board fully routed with a bottom GND pour
(`clean`), the same with a QFN at 45° (paste windows), a bottom-side THT header whose pin 1 joins the pour
through a thermal relief, 0402s at 30° and 135° (bottom) and an arc track (`features`), and one board per
deliberate violation: clearance track/track, track/pad, pad/via, short, net class width, minimum width, via
annular ring, via drill, pad drill and annular ring, hole-to-hole, copper-to-edge, courtyard overlap, silk over
pad, unrouted, keep-out (tracks), keep-out (footprints), and a netless track touching a net. Outputs are kept in
`$CARGO_TARGET_TMPDIR/{drc,gerber}_crosscheck/<board>/` for inspection.

**DRC** (`tests/drc_crosscheck.rs`): `drc::check` against `kicad-cli pcb drc --format json --severity-all
--refill-zones` on the export. KiCad types map to cadlab codes in one table (`TYPE_MAP`: `clearance`,
`shorting_items`/`tracks_crossing` → `drc.short`, `track_width`, `annular_width`, `drill_out_of_range`,
`hole_to_hole`, `copper_edge_clearance`, `courtyards_overlap`, `items_not_allowed` → `drc.keepout`,
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

Running locally (KiCad 10.0.6 and gerbv from Homebrew on macOS):
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
   diameter and position; a KiCad slot matched by a round hole at its middle is counted apart (the import
   drills slots round); hole functions are not compared (KiCad's component drill for a stitching-via
   footprint is cadlab's via drill).
6. **Re-route** (`CADLAB_CORPUS_ROUTE=1`, budget `CADLAB_CORPUS_ROUTE_BUDGET` seconds, default 60): tracks and
   vias ripped up and routed again by cadlab's router; completion is reported, never a failure.

Allowances, each with its reason, on top of the cross-check's (`drc_compare::ALLOW`):

- rules only one tool has (`ONE_SIDED_RULES` in `tests/corpus.rs`): KiCad's `silk_edge_clearance`,
  `text_height`, `text_thickness`, `solder_mask_bridge`, `footprint_type_mismatch`, `starved_thermal`,
  `copper_sliver`, `nonmirrored_text_on_back_layer`, `npth_inside_courtyard`, `pth_inside_courtyard`,
  `hole_clearance`; cadlab's `drc.footprint_outside`;
- KiCad `silk_over_copper` on board texts (cadlab checks silkscreen drawings, not texts);
- original board only: `drc.track_width_class` (KiCad never checks net class widths; the export adds a
  `.kicad_dru` rule, so the re-export does); cadlab findings of rules the project sets to `ignore` in its
  `.kicad_pro`; courtyard overlaps involving a footprint whose courtyard the import generated;
- re-export only: `drc.unplaced` (no schematic to check parity against);
- report granularity: KiCad reports silkscreen over copper per silkscreen segment, so extra KiCad findings on
  a pair cadlab reports are the same defect; KiCad's report stops at 199 findings of most types, so cadlab
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

Results with KiCad 10.0.6 at the pins: every board imports; the board-level comparison has no unexplained
difference; one board (`buspirate-flash-sop`) matches without any known entry. Routing everything again
completes 57-100 % within 30 s per board (`corne-cherry` 69 %, `tinytapeout-demo` 57 %: the router's
weakest cases).

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
dropped silently (now `import.local_setting`, `import.net_tie`, `import.tht_paste`).

**Known issues** (recorded per board in the manifest; candidates for later work):

- mask and paste margins (board, footprint, pad), local clearances and pad zone-connection overrides are not
  modeled; KiCad's solder mask minimum web (merged openings) is not either;
- net-tie footprints are ordinary footprints (their tied nets are shorts);
- pads on the other side of their footprint, copper drawings and texts inside footprints or on copper layers,
  footprints with designators cadlab cannot use (`POWER SW`), logos with a schematic symbol (unplaced), and
  board footprints missing from the schematic are not imported;
- paste on through-hole pads, slots (drilled round), trapezoid and chamfered pads (bounding rectangles);
- custom DRC rules with geometric conditions (`intersectsCourtyard`, `memberOfFootprint`, rule areas) and
  per-project rule severities;
- connectivity through pours differs where cadlab's refill (its own algorithm, no thermal spoke angle or count
  settings) does not reach a pad KiCad's fill reaches;
- not yet analyzed: a few residual differences on `buspirate5-rev10`, `corne-cherry`, `glasgow-revC3`,
  `glasgow-revD1`, `sweep-v2.2` and `tinytapeout-demo` (marked so in the manifest).

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

IPC-2581, STEP and IDF have golden files (`tests/golden/exchange/`) and structural checks that parse the files
back: a minimal XML reader (references to dictionaries and layers resolve, counts match the board), a Part 21
reader (every `#ref` defined, every B-rep shell closed: each edge used once in each direction) and an IDF section
reader. Two optional oracles skip when the tool is missing even with `CADLAB_ORACLES=1`:
`CADLAB_ORACLE_FREECAD` (`freecadcmd`, e.g. `/Applications/FreeCAD.app/Contents/Resources/bin/freecadcmd`) must
read the STEP file as valid closed solids whose volumes match the outline minus cutouts and holes, and the bodies'
boxes; `CADLAB_IPC2581_XSD` (a local copy of IPC's `IPC-2581C.xsd`, not shipped) validates the XML with
`xmllint --schema`. Example:
`CADLAB_ORACLES=1 CADLAB_ORACLE_FREECAD=/Applications/FreeCAD.app/Contents/Resources/bin/freecadcmd cargo test --test exchange`.

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

## Test corpus

- **Generated designs:** boards built by cadlab scripts (`tests/corpus/*.jsonl` command batches). These are fully
  ours and cover features systematically.
- **Open-source hardware projects:** fetched at CI time from their repositories (pinned commit), used as KiCad import
  and oracle inputs. Not vendored into the repo, and their licenses are recorded in the corpus manifest
  (`tests/corpus/projects.toml`, `scripts/fetch-corpus.sh`; see "Open-source corpus" above, D35).
- **Router benchmarks:** DSN files from the generated designs plus fetched projects; synthetic stress cases.
