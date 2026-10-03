# Autorouter

Goal: a native Rust router that replaces freerouting, usable as a library (`cadlab::router`, a candidate for spinning out as its own crate), from the CLI and from
MCP, with deterministic results and DRC-clean output.

**Licensing rule:** cadlab is MIT and freerouting is GPL-3.0. The router is designed from published algorithms
(papers, textbooks) only. freerouting's source must not be read for porting, copied or translated. freerouting is
used solely as an external benchmark oracle, run as a separate process on the same DSN input (DECISIONS D7).

## Inputs and outputs

```
Input:  board geometry (outline, keepouts), stackup, placed footprints + pads,
        existing copper (fixed or rip-able), nets + net classes, design rules, options
Output: new tracks/vias (+ optionally removed ones), per-net status, statistics, diagnostics
```

The router works on its own internal representation (`RouterBoard`), built from the model and converted back.
That keeps it independent and makes Specctra DSN/SES a natural second front end for benchmarking.

## Pipeline

```
1. Preprocess    obstacles per layer (pads, existing copper, keepouts, outline), inflated by clearance
                 connectivity: which pads are already connected; remaining connections via per-net MST
2. Order         sort connections: short/hard first, power last, user priorities, net class
3. Global route  coarse grid (tiles) to estimate congestion and choose layer/corridor per connection
4. Detail route  find DRC-valid paths within corridors (v1: grid A*, v2: gridless)
5. Negotiate     rip-up and reroute with rising congestion costs until convergence or budget exhausted
6. Optimize      pull-tight, remove redundant vias, smooth corners (45°/arcs), center between obstacles
7. Verify        full DRC on the result; anything failing gets ripped and retried or reported
```

## v1: grid-based (M5)

- Multi-layer routing grid at a resolution derived from rules (track + clearance), with off-grid pad access
  points connecting pads to the grid.
- A* with 8-direction moves (0°/45°/90°), costs for length, bends, vias, wrong-way direction per layer,
  congestion history.
- **Negotiated congestion** (PathFinder-style): all nets route allowing overlaps at a cost, then overlap penalties
  grow each iteration until no sharing remains. Usually converges much better than sequential rip-up.
- Vias as layer-transition edges, respecting via rules and keepouts. Through vias first, blind/buried later.
- Pros: simple, robust, easy to make correct. Cons: memory/time grows with board area/resolution, output looks
  "griddy" until optimized.

## v1 as implemented (`src/router/`)

`cadlab::router::route(project, scope, options, hooks)` returns new tracks and vias plus a report per
connection, without touching the project; the `route.*` commands apply it. Modules: `model` (the router's view of
the board), `grid` (grid and static maps), `engine` (A* and negotiation), `post` (optimization), `index` (spatial
index and exact clearance checks), `geo` (float geometry kernel). Algorithm modules only; nothing uses the command
layer.

**Preprocess.** Copper items and islands come from `crate::board` (zone fills included, so pads joined by a pour
count as connected). Every net's islands that hold a pad or via are joined by a minimum spanning tree (Prim, closest
anchor pair, as the ratsnest); already-connected copper is never re-routed and existing tracks stay. Obstacles:
other-net pads, tracks and vias with the larger of both nets' clearances, board edge and cutouts
(`copper_to_edge`), keep-outs (`no_tracks` per layer, `no_vias`), NPTH holes; drilled holes for via
`hole_to_hole`. Zone fills are not obstacles: they are refilled around the new copper. Net classes give each net
a *profile* (track width, clearance, via drill and diameter).

**Grid.** Pitch = (track width + clearance) / 2 of the finest routed profile (option `grid` overrides), so two
parallel tracks fit at two cells; the origin is shifted to put the most pad centers on cell centers. Maps are
sampled on the *double grid* (half pitch): even/even = cells, odd = midpoints of orthogonal and diagonal moves. A
move is legal when its three samples are legal. Static obstacles are inflated by
`δ = √(r² + s²/4) − r` (r: required distance, s: sample spacing, at most ~10 µm), which makes the sampling exact:
if two samples `s` apart are both ≥ r + δ from any shape, the whole segment is ≥ r away. Static maps per profile
and routing layer store free / blocked / *owned by net n* (near copper of net n only, so only n may pass); the via
map (cells, through vias, all copper layers) adds hole-to-hole and forbids vias in pads.

**Pad access.** Cells on a terminal's copper connect without a stub; otherwise up to 12 nearby cells with a
straight stub to the pad center (or onto an existing track) that passes an exact clearance check.

**Search.** Multi-source / multi-target A* over (layer, cell): 8 directions; costs: 1 per orthogonal step,
√2·1.05 per diagonal step, 1.8 for orthogonal steps against the layer's preferred direction (horizontal on even,
vertical on odd routing layers), 0.4 per 45° bend, 1.5 per 90° bend, sharper bends forbidden, 10 per through via;
octile heuristic to the target's bounding box (weighted 1.3 at low effort). A net is routed as a tree: each
connection starts from everything already connected to its source island.

**Negotiated congestion** (PathFinder, McMurchie & Ebeling 1995). Each committed net claims a *halo* per profile:
the points where another net's track centerline or via center would violate clearance (exact for grid geometry,
δ-inflated for off-grid stubs). Move cost = (base + history) × (1 + present × occupancy) + bend. Iteration 1
routes every net (shortest ratsnest first); then every overused point gets +0.5 history, the present factor
×1.6 (from 0.6), and the nets with overuse or near an overused point are ripped up and rerouted (order lightly
shuffled by the seeded RNG) until nothing is shared or the iteration budget is spent (10 / 40 / 120 by effort).
The best iteration is kept. **Legalization**: nets still conflicting are ripped (worst first) and rerouted with
sharing forbidden.

**Post-processing**, each step validated exactly against all other copper: via pairs whose layer section fits on
the outer layer are removed; collinear vertices merged; 45° pull-tight (a run of up to 10 vertices replaced by
the shorter two-segment octilinear path); remaining 90° corners chamfered. Junctions, vias and terminals are pinned.

**Verification.** `drc::check` runs on the project with the result; any new track or via in an error (other than
`drc.unrouted`) has its wire ripped and reported. The final status of every connection is read from the copper
islands of the verified board.

**Failure reports.** A failed connection is re-searched with obstacles passable at a penalty; what that path runs
through (exact checks) names the blockers: `no path from C2.1 to U1.5: blocked by keep-out `wall` on F.Cu`, with a
location, layer, subjects and hints (move a component, rip the routing in the way, allow more layers, lower the
clearance, raise the budget).

**Determinism and control.** No hash iteration, ties broken by node index, seeded RNG; progress per net and
iteration through a callback, cancellation polled every 4096 search expansions, and a time budget (negotiation
stops at 75%, legalization at 100%) that returns the best legal partial result. When the budget cuts a run short,
results may differ between runs.

**Commands.** `route.all {budget_ms?, layers?, effort?, seed?}`, `route.nets {nets}` (net or class names,
`class:power`), `route.connection {from, to}` (pins), `route.rip {nets? | all}` (unlocked items only),
`route.status` (connections, unrouted, completion %, unrouted by net, problem areas in 5 mm tiles). Default
budget 60 s. Commands report `route.incomplete` when connections fail.

### v1 limits

- Through vias only; no blind/buried vias, no via-in-pad.
- Tracks keep their class width everywhere: no neck-down into pads narrower than the track (pads whose
  surroundings leave no room report "no legal way out of ...").
- Grid-quantized: fine-pitch parts need a pitch fine enough to reach their pads (`grid` option, not yet exposed
  by the commands); no any-angle or arc output.
- No push-and-shove: existing copper is fixed; rip it (`route.rip`) to let the router redo it.
- A track crossing a pour can split it; the refill keeps DRC clean but the zone's net may show new ratsnest lines
  (route again).
- Hole-to-hole between vias of the same net is enforced only by the final DRC (the offending wire is ripped).
- Single-threaded; memory is a few bytes per double-grid point per layer and profile.

### Results (v1, release build, macOS development machine)

`cargo test --release --test route -- --ignored --nocapture` (`bench_generated_boards`); every result has zero DRC
errors. SOIC-16 grids: each IC joined to its right neighbor by a permuted 6-bit bus and to the one below by one
net, plus GND and VCC on all ICs.

| board | layers | nets | connections | completion | vias | length | iterations | time |
|---|---|---|---|---|---|---|---|---|
| LDO + 2 caps | 2 | 3 | 5 | 100% | 0 | 25.2 mm | 4 | 4 ms |
| dense: LQFP-32, SOIC-16, 2x8 header, crossing buses | 2 | 32 | 32 | 100% | 39 | 460 mm | 5 | 92 ms |
| 6× SOIC-16 grid | 2 | 29 | 37 | 100% | 33 | 346 mm | 5 | 50 ms |
| 12× SOIC-16 grid | 2 | 64 | 84 | 100% | 70 | 772 mm | 5 | 166 ms |
| 24× SOIC-16 grid | 2 | 140 | 184 | 100% | 151 | 1684 mm | 5 | 495 ms |
| 24× SOIC-16 grid | 4 | 140 | 184 | 100% | 149 | 1666 mm | 5 | 589 ms |

The ATtiny85 board of `tests/common` routes to 100% on 2 layers, auto-placed (22 connections, 11 vias) and hand
placed with a 0.5 mm power class (6 vias), in well under a second in debug builds. Boards can also be routed by
freerouting through the Specctra front end below (`tests/specctra.rs`); a metric comparison corpus is still to come.

## Specctra DSN/SES (`src/specctra/`)

External autorouters (freerouting and other Specctra-compatible ones) read a design file (DSN) and write a
session file (SES). cadlab writes the first and reads the second, implemented from the Specctra Design Language
Reference and session file description (D7, D25). Workflow:

```sh
cadlab export dsn out/route/board.dsn            # route it externally, e.g.
java -jar freerouting.jar -de out/route/board.dsn -do out/route/board.ses -mp 20 --gui.enabled=false
cadlab route import-ses out/route/board.ses     # then: cadlab drc run
```

**`export.dsn {path?, protect_existing?, resolution?}`** (`specctra::export`): `(resolution um 10)` by default
(the router's grid; coordinates themselves are written in µm with up to three decimals, i.e. exact nm). Written:

| DSN | from |
|---|---|
| `structure/layer` | copper layers, all `signal`, top to bottom |
| `boundary (path pcb 0 …)` | outer contour, arcs as chords within 1 µm |
| `keepout` on layer `signal` | each cutout (`cutout1`, ...) |
| `keepout` / `wire_keepout` / `via_keepout` | keep-outs forbidding tracks and vias / tracks / vias, one per layer (`signal` when all); pour- or footprint-only keep-outs are left out with a warning |
| `structure/via`, `structure/rule` | via padstacks in use (rules default first); rules track width and clearance |
| `placement` | each footprint: image = footprint name, position, `front`/`back`, rotation, `lock_type position` when locked, `PN` = part; mounting holes as components `H1`… (locked) |
| `library/image` | pins in footprint-local coordinates (pin ID = pad number; repeated or empty numbers become `<number>@<index>`), courtyard as outline, non-plated holes as image keep-outs; a non-plated mounting hole is an image with only a keep-out |
| `library/padstack` | one per distinct pad: `smd_…` on `F.Cu`, `tht_d<drill>_…` on every layer; rectangles as `rect` (a quarter turn swaps the sides), circles as `circle`, ovals as `path` with the minor width, round rectangles as polygons circumscribing the corner arcs (4 segments per corner), other pad angles as rotated polygons; vias `via_<diameter>_<drill>[_<from>-<to>]` in µm |
| `network/net`, `network/class` | nets with pins `U1-3`; one class per net class (track width, clearance, `use_via`) plus `default` from the rules |
| `wiring` | tracks as `wire (path …)` (arcs as chords), vias; `(type protect)` when locked, or all with `protect_existing` |

Back-side parts: Specctra mirrors the image across its Y axis and then rotates counter-clockwise by the
placement angle, which is cadlab's own transform, so rotations are written unchanged
(`specctra::dsn::place_point`). Zones are not written: they are refilled after import. Netless or unplaced
items stay out.

**`route.import_ses {path, keep_existing?}`** (`specctra::ses`): session coordinates are integers in steps of the
session's `(resolution <unit> <n>)` and convert exactly to nanometers (any unit: inch, mil, cm, mm, µm). Every
`network_out` net must exist in the circuit (`ses.unknown_net`, with suggestions), every wire layer must be a
copper layer (`ses.unknown_layer`). Wires (`path` only; others are counted in `ses.unsupported_wire`) become
track segments of the path's width; vias take diameter, drill and span from cadlab's padstack names, or the
diameter and layers from the session's `library_out` padstack and the drill from the net class or rules
(`ses.unknown_padstack` otherwise). Unless `keep_existing`, the unlocked tracks and vias of the session's nets
are removed first; locked items stay, and session wiring lying on them (within one resolution step; routers split
wires at junctions) is counted as duplicate instead of added. Placement differences between the session and the
board are reported (`ses.placement_mismatch`). The result gives counts and the unrouted connections left.

**Oracle** (`tests/specctra.rs`, `CADLAB_ORACLES=1 CADLAB_ORACLE_FREEROUTING=/path/freerouting.jar`, run with
`java -jar`): the ATtiny85 board (parts on both sides, rotated, holes, cutout, keep-out, a locked track) and the
STM32 board auto-placed on four layers are routed by freerouting 2.1 and imported; cadlab DRC must have no error
and no unrouted connection. freerouting 2.1 occasionally reports a complete route but leaves some wiring out of
its session (a net or a few connections missing from `network_out`); such runs are retried, up to 5 times. Also
tested without the oracle: a golden DSN, the reader round trip (equal data, every pin at its pad center through
`place_point`, nets complete), and a hand-written session (`tests/fixtures/ldo.ses`).

Limits: no DSN-to-project import (the reader is a library function used by tests); arcs (`qarc`) and polygon
wires in sessions are skipped; Specctra strings cannot contain `"` (written as `'`, with a warning); zones,
copper-to-edge and hole-to-hole rules are not expressed in the DSN (cadlab's DRC checks them after import).

## v2: gridless (M6)

- Free space represented by shapes instead of cells (polygon decomposition / expansion rooms, as in
  Specctra/freerouting-class routers), with search over room boundaries.
- Exact DRC by construction, with no grid quantization errors, which matters for fine-pitch parts.
- **Push-and-shove**: route a new track by moving existing ones out of the way while keeping them valid. Needed
  for incremental routing via API ("add this one net without destroying the rest").
- Fanout strategies for BGA/QFN (dog-bone, via-in-pad when allowed).
- Diff pairs routed as coupled pairs. Length matching via meander insertion (with M8).

## API surface

| Command | Purpose |
|---|---|
| `route.all` | route all unrouted connections |
| `route.nets` | route specific nets or net classes |
| `route.connection` | route between two pads/points, optionally with waypoints or a layer |
| `route.fanout` | fanout a component |
| `route.optimize` | post-process existing copper only |
| `route.rip` | remove routing (net, area, all non-locked) |
| `route.status` | unrouted connections, completion %, problem areas |

Options: time budget, max iterations, seed, layer restrictions per net class, via cost, allowed angles,
keep/rip existing copper, effort level.

The result always says what happened: routed/failed per connection, failure reasons with locations ("no path from
U3.12: blocked by J2 courtyard on F.Cu and via keepout on B.Cu"), and suggestions (move component, allow layer,
reduce clearance class). This turns an autorouter failure into something an agent can act on: adjust placement and
try again.

## Performance and determinism

- Seeded randomness only; same input + seed gives the same output.
- Parallelism: route independent regions/nets in parallel with deterministic merge order.
- Spatial indexing (R-tree) for obstacle queries, incremental updates during negotiation.
- Cooperative cancellation and progress reporting from inner loops.

## Benchmarks

`tests/corpus/router/`: DSN files from open-source boards, plus synthetic stress cases (dense QFP, BGA breakout,
2-layer tight). For each: completion %, via count, total length, DRC violations, runtime, recorded against
freerouting (run externally) and against the previous cadlab release. A regression in completion or DRC fails CI.
