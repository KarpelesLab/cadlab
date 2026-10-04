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
the board), `grid` (grid and static maps), `fanout` (BGA dog bones and fine-pitch escapes), `engine` (A* and
negotiation), `post` (optimization), `gridless` (visibility-graph refinement), `arcs` (arc corners), `shove`
(push-and-shove), `index` (spatial index and exact clearance checks), `geo` (float geometry kernel). Algorithm
modules only; nothing uses the command layer.

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
straight stub to the pad center (or onto an existing track) that passes an exact clearance check. Only pads,
not vias, of an island are ratsnest anchors when it has any (connections are reported pad to pad).

**Fanout and escape** (`fanout`, before everything else, on by default; option `fanout: false`, command
`route.fanout` to do it alone). *BGA dog bones*: a footprint whose pads fill at least half of a regular lattice of
at least 4 × 4 is an area array. Its outer ring (two rings when a track fits between neighboring balls) is left to
the router, which escapes it on the pad layer; every other ball of a net with something to route gets a short
track to a through via at the center of the four surrounding balls, on the diagonal pointing away from the array
center (quadrant fanout: a quadrant's vias line up and leave one-track channels between them on the inner layers),
falling back to the other diagonals. Vias never go in pads. *Fine-pitch escapes*: a component whose pads leave no
room for a track between neighbors, and of which at least one pad is too far off the routing grid for a track along
the nearest grid line to clear its neighbors, gets an escape for each off-grid pad: a straight track out along the
pad's long axis, then 45° onto a grid cell from which the router can move outwards (checked on the static maps),
the shallowest such cell first so that neighbors stagger. Escapes are planned greedily in pad order; pads left
without one go first in another attempt (up to four), and the attempt with the most escapes is kept (on the STM32
boards one LQFP-48 pad otherwise found every exit taken by its neighbors' escapes). Every stub and via is checked exactly against everything
placed so far, including the router's own sampling margin between escapes, so the fanout is DRC-clean by
construction. Fanout copper then counts as existing copper of its net for the run. In the output an escape is
absorbed into the routed path that uses it (so post-processing smooths the junction); fanouts that end up joining
their pad to nothing else are dropped.

**Search.** Multi-source / multi-target A* over (layer, cell): 8 directions; costs: 1 per orthogonal step,
√2·1.05 per diagonal step, 1.8 for orthogonal steps against the layer's preferred direction (horizontal on even,
vertical on odd routing layers), 0.4 per 45° bend, 1.5 per 90° bend, sharper bends forbidden, 10 per through via;
octile heuristic to the target's bounding box (weighted 1.3 at low effort). A net is routed as a tree: each
connection starts from everything already connected to its source island. A new via keeps `hole_to_hole` from the
vias of the net's earlier wires.

**Negotiated congestion** (PathFinder, McMurchie & Ebeling 1995). Each committed net claims a *halo* per profile:
the points where another net's track centerline or via center would violate clearance (exact for grid geometry,
δ-inflated for off-grid stubs). Move cost = (base + history) × (1 + present × occupancy) + bend. Iteration 1
routes every net (shortest ratsnest first); then every overused point gets +0.5 history, the present factor
×1.6 (from 0.6), and the nets with overuse or near an overused point are ripped up and rerouted (order lightly
shuffled by the seeded RNG) until nothing is shared, the iteration budget is spent (10 / 40 / 120 by effort) or
the best iteration is 12 iterations old. The best iteration is kept. **Legalization**: nets still conflicting are
ripped (worst first) and rerouted with sharing forbidden. **Rip-up and retry**: for each connection still failing,
a search with everything passable finds the routed nets in its way (their halos on its path); the failing net is
routed first and those nets again around it, kept when fewer connections fail, or (four times per run) when as
many fail but the failure moved to another net, which then gets its own retry (up to 8 nets, 6 rounds).

**Via minimization.** Negotiation keeps vias cheap (10 grid steps): dearer vias (15–30) cost completion on the
STM32, QFP and BGA boards of the benchmark, since layer changes are how congestion resolves. Afterwards every net with vias is
ripped and routed again without sharing, with vias four times as dear (40); the new routing is kept when it has
fewer vias, no more failed connections, and is at most 10 % plus 8 grid steps per saved via longer. Other nets
stay where they are, so the result is legal by construction. This saves 4–33 % of the vias for a few percent
more track.

**Batches and threads.** Each iteration's nets are split into batches by level scheduling on their *regions*
(terminal bounding box plus 1.5 mm and a quarter of its size): a net goes into the first batch after every batch
holding an earlier net whose region meets its own, at most 64 per batch. The nets of a batch are ripped up, routed
against the same occupancy (in parallel on rayon threads with the `parallel` feature, one search scratch per
thread) and committed in batch order. The batches depend only on the data, so the result is identical for any
number of threads, with or without the feature. Legalization and retries stay sequential.

**Post-processing**, each step validated exactly against all other copper: via pairs whose layer section fits on
the outer layer are removed; collinear vertices merged; 45° pull-tight (a run of up to 24 vertices replaced by
the shorter two-segment octilinear path); remaining 90° corners mitered with a 45° segment; with `any_angle`,
straight shortcuts at any angle (a run of vertices replaced by one segment when shorter and legal). Junctions, vias
and terminals are pinned; the grid cell where a stub or escape joins the path is not, so the junction straightens
too. At normal and high effort every net is optimized a second time once all nets are done, so nets optimized
early use the room freed by later ones.

**Gridless refinement** (`gridless`, on by default; option `gridless: false`). Every stretch of a path (up to 24
vertices between pinned ones) is replaced by the shortest path of a visibility graph when that is shorter: the
obstacles near the stretch (pads, keep-outs, edge, other nets' tracks and vias) are grown into *hulls*, their
convex hull widened by an octagon whose inradius is the distance the rules require from the centerline plus
0.5 µm, so a centerline along a hull keeps exactly the clearance. The hull vertices within 4 grid pitches of the
stretch that could shorten it (inside the ellipse through its ends whose size is its length; at most 240) are the
nodes; A* searches them with lazily validated edges: two-segment 0°/45°/90° connections (straight ones with
`any_angle`), each checked exactly. Classic visibility-graph shortest paths (Lozano-Pérez and Wesley 1979)
restricted to a corridor. Tracks then hug obstacles at minimum clearance instead of grid distance; the usual
pull-tight and mitering run again on what changed. On the benchmark it shortens tracks by 0.3–2.3 % with fewer
segments and no completion or DRC change, so it is the default.

**Arc corners** (`arcs`, off by default; `arc_radius`, default 1 mm). The last step: every bend that is not a via,
junction or terminal becomes a circular arc tangent to both segments (stored as the track's `mid`). At a corner
with interior angle φ an arc of radius r touches the segments at r / tan(φ/2) from the corner; it may use what the
previous arc left of the first segment and half of the next one (all of it when it ends at a pinned vertex). The
radius starts at the largest that fits, capped by `arc_radius`, and halves until the arc is legal, down to the
track width: the arc is checked as chords at most 0.1 µm from it with 0.1 µm more clearance, and inserted into the
index as those chords for later nets. Gerber (G02/G03), KiCad (`arc`) and IPC-2581 export arcs as arcs; the DSN
export writes them as chords within 1 µm. Lengths (`route.status`, stats, benchmark) are measured along arcs
(`board::track_length`).

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

**Commands.** `route.all {budget_ms?, layers?, effort?, seed?, fanout?, any_angle?, gridless?, arcs?,
arc_radius?}`, `route.nets {nets, ...}` (net or class names, `class:power`), `route.connection {from, to, shove?,
...}` (pins), `route.track {layer, points, net?, width?, mode?}` (push-and-shove placement, below),
`route.fanout {refdes?, layers?}`, `route.rip {nets? | all}` (unlocked items only), `route.status` (connections,
unrouted, completion %, unrouted by net, problem areas in 5 mm tiles). Default budget 60 s. Commands report
`route.incomplete` when connections fail.

**Push-and-shove** (`shove`). Places new copper (the *head*: track polylines and vias) and moves the unlocked
tracks and vias of other nets out of its way, keeping every rule. Designed from the general walkaround / shove
idea of interactive routers, not from any router's source:

- *World.* Pads, locked tracks and vias, arc tracks, copper of the head's own nets, keep-outs, holes and the board
  edge are fixed. Unlocked straight tracks of other nets become *lines*: chains of segments of one net and layer
  between anchors (a via, a pad, a junction of three or more segments, a free end). Unlocked vias of other nets
  move unless they sit on fixed copper of their net or hold a line that cannot move. Locked items never move.
- *Hulls.* Anything in the way is grown into its hull: the convex hull of its shape widened by an octagon whose
  inradius is the required distance plus 0.5 µm, so the hull's edges keep the clearance exactly (0°/45°/90° edges
  for octilinear copper).
- *Walkaround.* A line that runs into something it may not move is bent around its hull: the part between its
  first entry into and its last exit from the hull is replaced by the hull boundary, on the side with the fewest
  remaining conflicts, then the shorter. An end inside the hull cannot walk around.
- *Shove.* Lines hit by a pushing item are walked around its hull (only the pushing segments near the hit, so a
  long line does not push with one coarse hull); vias hit are moved to the nearest point outside it, dragging the
  ends of their lines along; vias go first, since moving them often clears the lines. Every moved item first gets
  clear of fixed things (walkaround, or a further push for a via), then pushes in turn, first in first out, until
  nothing collides; at most 600 pushes, else `route.shove_limit`. A line whose fixed end is in the way, or that
  crosses the head (a topological crossing), cannot be shoved (`route.shove_failed`, naming it).
- *Spring-back.* Moved lines go back to their original geometry when that is legal again, otherwise they are
  pulled tight (45° pull-tight and mitered corners) against everything else; moved vias go back when they and their
  lines can. Two passes.
- *Check.* Everything new or moved is checked exactly against everything else, then the DRC runs on the result:
  nothing is returned that it flags on a new item (`route.shove_drc`).

`route.track {layer, points, net?, width?, mode?}` (`router::place_track`) places one track along waypoints
(coordinates or pins) this way: `mode: shove` (default) walks around fixed things and shoves the rest,
`walkaround` walks around everything and moves nothing, `strict` places the track exactly or fails
(`route.track_blocked` with what is in the way, where, and a hint: move the waypoints, unlock the copper, another
mode). The output lists the nets shoved and whether the track detoured.

`route.connection` (`shove`, on by default): when the connection fails, it is routed again with the movable copper
of other nets *softened*, first touching allowed (no clearance to it), then only its centerline in the way (so
the new route never crosses it), and that copper is shoved out of the new route's way; the first attempt that
passes the checks wins (arcs are left out of such a route). Otherwise the older fallback runs: the nets named in
the failure report are ripped and routed again around the connection (up to 3 rounds), kept only when they end
with no more unrouted connections than before. `route.all` and `route.nets` give every connection they leave
unrouted one push-and-shove attempt on the routed board, within the time budget (this closed the last STM32 gap).
The result lists the nets moved (`rerouted`) and replaces their old copper.

### v1 limits

- Through vias only; no blind/buried vias, no via-in-pad (dog bones only).
- Tracks keep their class width everywhere: no neck-down into pads narrower than the track (pads whose
  surroundings leave no room report "no legal way out of ...").
- The search is on the grid: off-grid fine-pitch pads rely on escapes, and gridless refinement only shortens what
  the grid search found (it never finds a path the grid missed). Arcs only as corner rounding (`arcs`).
- Push-and-shove moves straight unlocked tracks and vias; arc tracks, lines of mixed widths and closed loops stay
  fixed, vias are pushed as through vias, lines are not split or merged, and a free end (a track ending on
  nothing) cannot move. A shoved line keeps its layer.
- A track crossing a pour can split it; the refill keeps DRC clean but the zone's net may show new ratsnest lines
  (route again).
- Hole-to-hole between vias of the same net in *one* wire of a search is left to the final DRC.
- Parallelism is per batch of nets with disjoint regions: compact boards, where most nets meet near the same
  parts, get little of it. Memory is a few bytes per double-grid point per layer and profile, plus one search
  scratch (17 bytes per node) per thread.

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
freerouting through the Specctra front end below (`tests/specctra.rs`); the M6 benchmark suite is under
"Benchmarks" at the end.

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
  Specctra/freerouting-class routers), with search over room boundaries. *Done so far:* gridless refinement of
  grid routes (visibility graph over clearance hulls, above); the search itself is still the grid's.
- Exact DRC by construction, with no grid quantization errors, which matters for fine-pitch parts.
- **Push-and-shove**: route a new track by moving existing ones out of the way while keeping them valid. Needed
  for incremental routing via API ("add this one net without destroying the rest"). *Done:* `route.track`,
  `route.connection` and leftovers of `route.all` / `route.nets` (above).
- Fanout strategies for BGA/QFN (dog-bone, via-in-pad when allowed). *Done:* dog bones and escapes (D28).
- Diff pairs routed as coupled pairs. Length matching via meander insertion (with M8).

## API surface

| Command | Purpose |
|---|---|
| `route.all` | route all unrouted connections |
| `route.nets` | route specific nets or net classes |
| `route.connection` | route between two pads, pushing unlocked copper of other nets aside (`shove`) |
| `route.track` | place a track along waypoints with push-and-shove (walkaround, shove, spring-back) |
| `route.fanout` | fanout a component: BGA dog bones, fine-pitch escapes |
| `route.optimize` | post-process existing copper only (to come) |
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
- Parallelism: route independent regions/nets in parallel with deterministic merge order (batches of nets
  with disjoint regions, see above).
- Spatial indexing (R-tree) for obstacle queries, incremental updates during negotiation.
- Cooperative cancellation and progress reporting from inner loops.

## Benchmarks

**Suite** (`tests/common/routebench.rs`): deterministic generated boards, easiest first, each routed from
scratch with `route.all {seed: 1, budget_ms: 120000}`:

| board | what |
|---|---|
| `attiny-2l` | ATtiny85 board (SOIC-8, SOT-23-5, 0402/0603, headers), auto-placed, 2 layers |
| `soic24-2l` | 24 SOIC-16s with permuted 6-bit buses between neighbors, GND and VCC on all, 2 layers |
| `stm32-2l` | STM32F103 (LQFP-48, 0.5 mm) board with USB, LDO, crystal, headers, auto-placed, 2 layers |
| `stm32-4l` | the same on 4 layers |
| `qfp-qfn-4l` | three cells of the synthetic board: LQFP-100, LQFP-64, QFN-48, 0402/0603 passives on both sides, headers, GND/3V3/1V8/5V pours on the inner layers, 0.15 mm rules, 4 layers |
| `fine-4l` | QFN-48 (0.5 mm), QFN-40 (0.4 mm), 2× TSSOP-28, TSSOP-20, scrambled signals, 0.1 mm rules, 4 layers |
| `bga144-4l` | BGA-144 (12 × 12, 0.8 mm) with every signal ball to one of four 2 × 15 1.27 mm headers, GND/VCC balls, 0.1 mm rules, 4 layers |
| `big-4l` | the whole 160 × 100 mm synthetic board (`bigboard.rs`: ~500 parts, ~1300 connections), routing removed |

Metrics: connections (pads − 1 per net) and completion from `route.status`, vias, track length and segments,
*sharp corners* (two segments of a net meeting at 90° or less with nothing else there), wall-clock time, and DRC
errors other than unrouted connections (they must be 0). `cargo run --release --example route_bench [names...]`
prints the table; `cargo test --release --test route_bench -- --ignored --nocapture` also asserts zero DRC errors.
With `CADLAB_ORACLE_FREEROUTING=/path/freerouting.jar` (and `JAVA_HOME` for the Java it needs) every board but
`big-4l` is also routed by freerouting (external process on the exported DSN, `-mp 20 -mt 1`; the time includes
the JVM start and file exchange) and imported through `route.import_ses`, then measured the same way.
`CADLAB_ROUTE_RENDER=<dir>` renders cadlab's results.

**Results** (release build, 16-core macOS development machine, freerouting 2.4.1 on Java 25). *M5* is the router
at the start of M6, *phase 1* the first M6 version (fanout, batches, rip-up and retry), *phase 2* this one (escape
retries, via minimization, sideways retries, push-and-shove of leftovers, gridless refinement); freerouting's
numbers are from the phase 1 run (same boards, unchanged).

| board | router | connections | completion | vias | length | segments | sharp corners | time |
|---|---|---|---|---|---|---|---|---|
| attiny-2l | M5 | 22 | 100% | 6 | 104.5 mm | 62 | 0 | 15 ms |
| | phase 1 | 22 | 100% | 6 | 104.4 mm | 58 | 0 | 19 ms |
| | phase 2 | 22 | 100% | 4 | 104.1 mm | 56 | 0 | 18 ms |
| | freerouting | 22 | 95.5% | 1 | 117.0 mm | 59 | 4 | 3.7 s |
| soic24-2l | M5 | 184 | 100% | 152 | 1678.7 mm | 586 | 0 | 182 ms |
| | phase 1 | 184 | 100% | 152 | 1677.0 mm | 577 | 0 | 185 ms |
| | phase 2 | 184 | 100% | 141 | 1690.6 mm | 578 | 0 | 268 ms |
| | freerouting | 184 | 100% | 78 | 2195.6 mm | 707 | 26 | 6.1 s |
| stm32-2l | M5 | 109 | 88.1% | 64 | 767.1 mm | 378 | 0 | 217 ms |
| | phase 1 | 109 | 98.2% | 78 | 912.6 mm | 457 | 1 | 743 ms |
| | phase 2 | 109 | **100%** | 57 | 968.9 mm | 453 | 0 | 473 ms |
| | freerouting | 109 | 100% | 48 | 1111.9 mm | 462 | 7 | 9.2 s |
| stm32-4l | M5 | 109 | 86.2% | 45 | 696.8 mm | 333 | 1 | 322 ms |
| | phase 1 | 109 | 98.2% | 61 | 827.5 mm | 401 | 2 | 1.3 s |
| | phase 2 | 109 | **100%** | 55 | 889.8 mm | 409 | 2 | 3.1 s |
| | freerouting | 109 | 99.1% | 43 | 973.1 mm | 363 | 8 | 9.8 s |
| qfp-qfn-4l | M5 | 155 | 98.1% | 144 | 1959.4 mm | 737 | 0 | 2.2 s |
| | phase 1 | 155 | 100% | 147 | 1970.3 mm | 711 | 0 | 1.9 s |
| | phase 2 | 155 | 100% | 128 | 2000.2 mm | 709 | 0 | 2.6 s |
| | freerouting | 155 | 99.4% | 164 | 2370.2 mm | 815 | 12 | 20.4 s |
| fine-4l | M5 | 100 | 100% | 116 | 812.6 mm | 409 | 0 | 718 ms |
| | phase 1 | 100 | 100% | 116 | 812.5 mm | 407 | 0 | 690 ms |
| | phase 2 | 100 | 100% | 111 | 812.9 mm | 400 | 0 | 924 ms |
| | freerouting | 100 | 100% | 116 | 1033.5 mm | 550 | 5 | 12.4 s |
| bga144-4l | M5 | 141 | 97.2% | 173 | 2228.0 mm | 939 | 1 | 11.1 s |
| | phase 1 | 141 | 100% | 172 | 2238.8 mm | 871 | 1 | 6.8 s |
| | phase 2 | 141 | 100% | 146 | 2209.3 mm | 872 | 2 | 8.3 s |
| | freerouting | 141 | 95.0% | 122 | 2368.5 mm | 945 | 3 | 153.6 s |
| big-4l | phase 1 | 1275 | 99.7% | 1301 | 21127.9 mm | 6332 | 1 | 96.8 s (budget) |
| | phase 2 | 1275 | 99.6% | 1141 | 21382.2 mm | 6261 | 1 | 112 s (budget) |

Every run has zero DRC errors (freerouting's sessions included, as cadlab checks them). cadlab now matches or
beats freerouting's completion on all seven comparable boards, with 7–23 % shorter tracks, fewer sharp corners
and a fraction of the time. Phase 2 closed the STM32 gaps: one
LQFP-48 pad on each board had no escape left (its neighbors' escapes took every exit; fixed by the escape
retries), and a congestion leftover on the 4-layer board is now resolved by push-and-shove. Vias dropped by
4–33 % (via minimization): fewer than freerouting's on qfp-qfn-4l and fine-4l, still more on the 2-layer boards
and bga144-4l. Track length stays within 1.5 % of phase 1 (gridless refinement wins back most of what via
minimization adds; bga144-4l ends 1.3 % shorter) except on the STM32 boards (+6–8 %), which now also route their
last connections, with fewer vias. Times grew by the via-minimization pass and, on stm32-4l, the push-and-shove of
one leftover connection (about 1.7 s, two softened routing runs). `big-4l` exhausts its 120 s budget either way
(negotiation stops at 75 %), so its numbers depend on the machine: phase 2 saves 160 vias there for 1.2 % more
track, and its completion is within one connection of phase 1; batches there hold about three nets on average,
which bounds the parallel speedup: long single-connection nets across the board dominate the time.

With arcs (`route.all {arcs: true, arc_radius: 0.8mm}`) the ATtiny board routes the same with its bends rounded,
DRC-clean, exported as arcs to Gerber and KiCad (`tests/route.rs`, `arc_corners_are_drc_clean_and_exported`).

The older `bench_generated_boards` in `tests/route.rs` (M5 table above) still runs the SOIC grids and the dense
LQFP-32 board. Still to come: DSN files of open-source boards as a corpus, and a CI job that fails on a
completion or DRC regression.
