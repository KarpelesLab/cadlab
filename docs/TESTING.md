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
  by layer, by aperture/flash count and by drill hit count.
- **DRC:** compare violation sets by (type, objects involved, location within tolerance). Known semantic
  differences go in a reviewed allowlist with a reason for each entry.
- **Routing:** metric comparison only (completion %, vias, length, time), with regression thresholds.
- **Schematic** (`tests/kicad_oracle.rs`, from M3): the ATtiny85 board is exported with `schematic.export`, then
  `kicad-cli sch erc` must report no errors (allowlisted warnings: `lib_symbol_issues` and
  `footprint_link_issues`, because the embedded `cadlab`/`cadlab_power` libraries are not in KiCad's library
  tables), and `kicad-cli sch export netlist` must give every cadlab net exactly the same (ref, pin) nodes,
  under the same name (KiCad prefixes local labels with the sheet path `/`). A variant with rotated box symbols
  checks the netlist only. Tested with KiCad 10.0.6. Example:
  `CADLAB_ORACLES=1 CADLAB_ORACLE_KICAD_CLI=/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli cargo test --test kicad_oracle`.

### Oracle availability

- Oracle tests are behind an env flag (`CADLAB_ORACLES=1`). Without it they skip with a message; with it, a
  missing tool fails the test, so CI never skips silently. Tool paths can be overridden with
  `CADLAB_ORACLE_KICAD_CLI`, `CADLAB_ORACLE_FREEROUTING`, ... (`tests/common/mod.rs`).
- Golden files live next to the tests (`tests/golden/`); `CADLAB_BLESS=1 cargo test` rewrites them. Snapshot
  tests use `insta` (`cargo insta review`).
- CI runs them in a container with pinned versions of each oracle, so results are reproducible.

## Test corpus

- **Generated designs:** boards built by cadlab scripts (`tests/corpus/*.jsonl` command batches). These are fully
  ours and cover features systematically.
- **Open-source hardware projects:** fetched at CI time from their repositories (pinned commit), used as KiCad import
  and oracle inputs. Not vendored into the repo, and their licenses are recorded in the corpus manifest.
- **Router benchmarks:** DSN files from the generated designs plus fetched projects; synthetic stress cases.
