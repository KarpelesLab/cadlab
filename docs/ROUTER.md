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
