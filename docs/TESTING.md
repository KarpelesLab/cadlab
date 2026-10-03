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
| **ngspice** | SPICE netlist export parses and simulates (M8) |
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

### freerouting oracle (`tests/specctra.rs`)

Two boards are exported with `export.dsn`, routed by freerouting headless (`-mp 20 -mt 1`, a separate `java`
process) and imported with `route.import_ses`; cadlab DRC must then report no error, unrouted connections
included. This checks the DSN geometry end to end (a wrong back-side transform shows up as shorts and opens) and
the exact unit conversion of the session. `CADLAB_ORACLE_FREEROUTING` points at the jar (run with `java -jar`)
or a launcher; `CADLAB_SPECCTRA_KEEP=<dir>` keeps the DSN, SES and a render. Tested with freerouting 2.1.0, which
sometimes drops wiring from its session while reporting a complete route; those runs are retried (details in
ROUTER.md). Not in the CI oracle job, which installs no Java. Example:
`CADLAB_ORACLES=1 CADLAB_ORACLE_FREEROUTING=$HOME/freerouting.jar cargo test --test specctra -- --nocapture`.

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
  and oracle inputs. Not vendored into the repo, and their licenses are recorded in the corpus manifest.
- **Router benchmarks:** DSN files from the generated designs plus fetched projects; synthetic stress cases.
