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
the board), `grid` (grid and static maps), `fanout` (BGA dog bones, fine-pitch escapes, neck-downs), `engine`
(grid A* and negotiation), `rooms` (free-space decomposition) and `expansion` (the gridless search over it), `post`
(optimization), `pours` (zone fills as places to connect to), `gridless` (visibility-graph refinement), `arcs`
(arc corners), `shove` (push-and-shove), `index` (spatial index and exact clearance checks), `geo` (float
geometry kernel). Algorithm modules only; nothing uses the command layer.

**Preprocess.** Copper items and islands come from `crate::board` (zone fills included, so pads joined by a pour
count as connected). Every net's islands that hold a pad or via are joined by a minimum spanning tree (Prim, closest
anchor pair, as the ratsnest); already-connected copper is never re-routed and existing tracks stay. Obstacles:
other-net pads, tracks and vias with the larger of both nets' clearances, board edge and cutouts
(`copper_to_edge`), keep-outs (`no_tracks` per layer, `no_vias`; not for the nets a keep-out excepts, see
"Keep-out net exceptions"), NPTH holes; drilled holes for via `hole_to_hole`. Zone fills are not obstacles: they are
refilled around the new copper (and checked again for what that cuts off, see Pours). Net classes give each net
a *profile* (track width, clearance, via drill and diameter).

**Scoped rules** (`scoped`, D61). The router keeps the clearance the DRC computes for each pair of items
(`board::clearance`): the last custom rule holding either item (rules between two scopes included), else the
larger local clearance, else the larger net clearance, never under `min_clearance`. Existing copper gets its
rules exactly. For new copper they depend on where it is, so every rule scope that can hold a track or via (its
kinds, layer, courtyards, areas) is a *condition*, and new copper has a state per condition: surely held (a track
centerline 1 µm inside a courtyard, 5 µm plus its half width inside an area for "entirely inside"), surely not
(1 µm beyond its own reach), or maybe (the band between, or the uncertainty of where the copper is: a grid sample
covers every point of a move through it). The clearance used is the largest the DRC could resolve for any
combination of the maybes, so it is never too small. The grid's static maps and halos read a state raster per
layer (and per via cell); exact checks (`index`, gridless, post-processing, push-and-shove) split each segment into
pieces of uniform state (bisection to 20 nm) and check each piece. The DRC decides per item, and a track crossing
a boundary would be decided as a whole (one that merely reaches into a courtyard is held everywhere, one partly
outside an area nowhere), so routed tracks are split where their state changes: every output track is held
uniformly and decided as the router assumed. Plated holes keep `hole_clearance` from other nets' copper (D56):
holes of pads and existing vias are obstacles at that distance, and a new via keeps the larger of its pad plus the
clearance and its drill plus the hole clearance. Fanout heuristics (rings left to the router, fine-pitch rows,
neck-down widths, never under a track width rule that may hold the neck) use the clearance in effect near the pad.
Without custom clearance rules, local clearances or a hole clearance every value is the classes' as before (the
benchmark baseline is identical).

**Grid.** Pitch = (track width + clearance) / 2 of the finest routed profile (option `grid` overrides), so two
parallel tracks fit at two cells. When the pad pitch most fine-pitch rows share (at least 8 pads of multi-pin
parts, no track between neighbors) is `2m` times a pitch at most 6 % coarser, the grid takes that pitch instead:
every pad of such a row then sits at the same offset from the grid, so the escapes of neighbors stay a pad pitch
apart (with a drifting offset, two neighbors end up one grid line apart somewhere along each row, and one of them
is walled in: half the pads of corne-cherry's 0.4 mm QFNs had no escape at 0.195 mm). The origin is shifted, per
axis, to put on grid lines the most pads of the finest rows running along that axis (rows that would need
escapes off the lines and whose pad pitch is a multiple of the grid's, so one offset serves the whole row), then
the most pad centers on cell centers (D84: the majority of all pads used to decide alone, which left
tinytapeout-demo's QFN-80 0.0925 mm off the lines; every pad then has to turn onto a line within the row, two
neighbors turning the same way at the same depth are 0.28 mm apart on their diagonals where 0.385 mm is needed,
two turning towards each other end on adjacent lines, and 10 of its 75 pads got no exit. Other pads are reached
by a stub from any cell, so they only break ties). Maps are
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
the shallowest such cell first so that neighbors stagger. Escapes are planned for the whole package before any is
committed (D64): every pad gets its candidate exits (up to 24, shallowest first, end cells at most two pitches
beside the pad's axis, each legal against everything placed so far), and one exit per pad is chosen as an
assignment over them: pads in order take their best candidate free of the exits chosen so far, then every pad
left without one tries its candidates in turn, moving the chosen exits in its way (at most three) to other
candidates of theirs, which may in turn move one more in their way, up to two levels (ejection chains), until no
pad gains an exit. Two candidates clash when their tracks break a clearance or one ends in the other's halo as the
router samples it. A greedy pass in pad order walls in every other pad of a tight row: the 0.4 mm QFN-80 of
tinytapeout-demo got 50 exits for 75 pads, now 65; an RP2040 of corne-cherry 22 of 28, now 27. A pad with no such
candidate tries the turn earlier, then straight on to the cell (D69: a neighbor's exit turning towards it further
out leaves it no room to turn at the cell's depth), and one still without tries a turn by less than onto a grid
line (multiples of 50 µm to either side), then straight on and a stub onto a cell further out with five free
cells ahead (D84: corne-cherry's CC1 between a no-connect pad and the D− exit turning towards it gets out with a
turn by 0.25 mm, which ends on no grid line). Every stub and via
is checked exactly against everything placed so far, including the router's own sampling margin between escapes,
so the fanout is DRC-clean by construction. *Crossover vias*: in a row of fine-pitch single-layer pads, two nets whose pads interleave tightly
(`X Y X Y`, the D+/D− pairs of a USB-C receptacle) cannot both join their pads on that layer: whichever joins
first walls in the other's inner pad. The inner pad of one of them gets a via straight out of the pad, the
shallowest from which both its neighbors still leave the row (straight, then at 45° away from it, past it, turning
as late as the via allows) and the pads beyond those neighbors can still turn away before the neighbors' exits turn
towards them (D69: on corne-cherry's receptacles the shallowest via walled in CC1 and the D+ pad beyond the
crossing); those exits are reserved as escapes, and the net's other pad, beyond one of them, gets an exit of its own that
keeps clear of it and ends in a via when the router could not reach it from a grid cell otherwise. The other net then joins its pads around the via on the pad layer, and this
one joins its two vias on another layer (the way such receptacles are routed by hand). That loop is not checked:
on nrfmicro a diode pad 1.9 mm behind the receptacle's pads leaves 0.18 mm between a 0.2 mm track around the via
and itself, 0.2 mm asked, and the router has to find another way (it does on a finer grid, see "Finer grids";
planning vias for both nets there was tried and dropped, D84). Fanout copper then counts
as existing copper of its net for the run. In the output an escape is
absorbed into the routed path that uses it (so post-processing smooths the junction); fanouts that end up joining
their pad to nothing else are dropped.

**Search.** Multi-source / multi-target A* over (layer, cell): 8 directions; costs: 1 per orthogonal step,
√2·1.05 per diagonal step, 1.8 for orthogonal steps against the layer's preferred direction (horizontal on even,
vertical on odd routing layers), 0.4 per 45° bend, 1.5 per 90° bend, sharper bends forbidden, 10 per through via;
octile heuristic to the target's bounding box (weighted 1.3 at low effort; with more than two routing layers,
the smaller of the distance to the targets on the node's own layer and a via plus the distance to any target, D72);
the queue is a 4-ary heap of
(cost, node) keys, popping in the same order as a binary heap would. A net is routed as a tree: each
connection starts from everything already connected to its source island. A new via keeps `hole_to_hole` from the
vias of the net's earlier wires.

**Negotiated congestion** (PathFinder, McMurchie & Ebeling 1995). Each committed net claims a *halo* per profile:
the points where another net's track centerline or via center would violate clearance (exact for grid geometry,
δ-inflated for off-grid stubs). Move cost = (base + history) × (1 + present × occupancy) + bend. Iteration 1
routes every net (shortest ratsnest first); then every overused point gets +0.5 history, the present factor
×1.6 (from 0.6, capped at 1000 as VPR's `max_pres_fac`: beyond it sharing is prohibitive anyway, and a
higher factor only makes the searches of the few nets still in conflict visit ever more of the board), and the nets with overuse or near an overused point are ripped up and rerouted (order lightly
shuffled by the seeded RNG) until nothing is shared, the iteration budget is spent (10 / 40 / 120 by effort) or
the best iteration is 12 iterations old. *Partial rip-up* (D69): a net of 64 connections or more (ground and supply
nets) that is rerouted only because its box holds an overused point, with no overuse of its own, keeps its wires
whose box, grown by the clearance reach and ten grid pitches, holds no overused point; only the others are ripped
and routed again from what is kept (kept wires must still reach the net's pads through each other, else they are
ripped too). A net in conflict is routed again whole, as are all nets in legalization and retries. Rerouting the
whole ground net every iteration, because its box holds every overused point, was 60 % of glasgow-revC3's
negotiation (GND, 184 connections, 1.3 s per iteration) and 30 % of glasgow-revD1's. *Costs near the targets* (D64): late in negotiation (present factor 5 or
more, from the sixth iteration) the history and present costs around the targets are a cost every path pays,
which the distance heuristic does not see, so the searches flood: a single connection of corne-cherry visited
350 000 nodes. A search still running after 10 000 expansions settles the 5 000 nodes nearest its targets backwards
(Dijkstra over the moves it may make, on its costs without bends) and from then on estimates each node by the
larger of the distance and its settled cost, capped at the last cost settled (a lower bound for every node not
settled). The estimate stays a lower bound and consistent, so paths stay optimal; entries already queued keep
their smaller keys, which only brings them out earlier. It halves the negotiation search time on corne-cherry and
cuts it to a third on cynthion (same searches, before and after). The best iteration is kept. **Legalization**: nets still conflicting are
ripped (worst first) and rerouted with sharing forbidden. **Rip-up and retry**: for each connection still failing,
a search with everything passable finds the routed nets in its way (their halos on its path); the failing net is
routed first and those nets again around it, kept when fewer connections fail, or (four times per run) when as
many fail but the failure moved to another net, which then gets its own retry (up to 8 nets, 6 rounds).
*Stubs*: an access stub is off the grid, and the halos see it only at the grid's samples, so two stubs of different
nets could come too close between samples (the DRC then removed one of the connections). Every grid wire's stubs
are checked exactly against the other nets' off-grid copper (stubs, gridless wires) when conflicts are counted (grid
copper is in the halos already; counting it again changed negotiation for nothing), and searches that may not share
(legalization, retries) drop source and target stubs that are not clear of any committed routing (D64).
*Re-planned escapes*: when a run leaves a fine-pitch pad without an exit and its connection fails, the nets of that
pad and of its package's pads within three pitches (nets of at most 8 connections) are ripped up and routed again
with the boxed pads served first by the escape planning; kept when fewer of their connections fail (D64; needs a
twentieth of the time budget left). When such connections still fail after rip-up and retry, the gridless phases
and via minimization of the first run stop early by twice the run's setup time (fanout, board, grid and pad
access, most of what the re-plan costs), when that is at most a quarter of the budget, so the re-plan gets that
time (D69: on tinytapeout-demo the gridless rip-up and retry used to run to the end of the budget).

**Via minimization.** Negotiation keeps vias cheap (10 grid steps): dearer vias (15–30) cost completion on the
STM32, QFP and BGA boards of the benchmark, since layer changes are how congestion resolves. Afterwards every net with vias is
ripped and routed again without sharing, with vias four times as dear (40); the new routing is kept when it has
fewer vias, no more failed connections, and is at most 10 % plus 8 grid steps per saved via longer. Other nets
stay where they are, so the result is legal by construction. This saves 4–33 % of the vias for a few percent
more track.

**Pours** (`pours`, D80). A zone fill is derived data: it joins the pads it covers when the run is planned, and it
is filled again around every new track and via afterwards. The plan used to take the first for granted: pads a
pour joined on the board as given needed no connection, tracks of other nets then cut the pour between them, and
the board came out with open connections the run never counted (on the corpus re-route, where the board starts
empty and every surface pour is whole, most of what was left open: buspirate5-rev10 1 failed and 66 open). Three
things handle it:

- *Fills are places to connect to.* Each fill island of a net on a routing layer is rasterized into the grid
  cells where a track of the net ending at the cell center lies on its copper (the center at least the half
  width plus 20 µm inside). A connection whose far side holds fills (they are part of that side's copper island)
  may end on any such cell within 4 mm of its near end (at least 16 cells), besides the side's pads and tracks:
  the usual via into a plane or short track into a pour. When only the tree's side holds fills the search runs
  from the other side and the path is turned around.
- *Pour rounds.* Once negotiation and legalization leave a legal routing, the fills are computed for it
  (`board::copper_items` on the board with the wires as they stand). A net with fills is planned again, from
  what the fills alone still join, when they no longer join an island of its plan, or when one of its
  connections is routed and yet open (a track ending on a piece of pour that was cut off since). Its routing is
  dropped and negotiation starts again with those nets to route (present factor as it was, history kept), so
  the nets around a cut-off pad make room for its via or track; then legalization, and the fills again: up to 3
  rounds before the retries and via minimization, up to 2 after them (they move nets too). A round that ends
  with more connections open on the filled board than before it is undone (everything back to the routing
  before the round, no more rounds), so a round cut short by the budget costs nothing. A first negotiation
  still going at 55 % of the budget (it ends at 75 %) fills the pours for the routing as it stands, shared points
  and all: with one pad cut off per 100 connections or more it ends there, and the rounds get the rest of its
  share; with fewer it goes on (glasgow-revC3: 4 pads cut off in 739 connections, where ending early cost seven
  connections). Tried and dropped: planning the cut-off nets again inside the running negotiation, with nothing
  to go back to (glasgow-revD1 ended with 416 connections open instead of 120–137), and negotiating again for
  everything legalization left failing after ending early (worse on four of six boards).
- *Honest counts.* The final status of every connection was already read from the copper islands of the
  verified board; a net with more ratsnest lines open there than it has failed connections now reports the
  difference as failed connections ("cut off from the pour of net GND", with the via or track to add as hints),
  so `stats.failed` no longer undercounts what `route.status` and the DRC then show.

Limits: a net is planned again whole (its wires that were fine are routed again); fills are those of the legal
routing before optimization, so a pull-tight or a via removed afterwards can still cut a neck (reported, not
fixed); a pour of a net with a rule profile no routed net uses is not planned again (reported). Boards without
zones route exactly as before.

**Searches that cannot succeed.** A search that fails visits everything it can reach from its sources first,
often most of the board. Two exact tests skip them without changing any result (D57). *Static reachability*: per
rule profile, the connected components of the grid graph over the points every net may use (union-find, once,
on first use), merged per net with the points only it may use (near its own pads and copper); negotiation passes
other nets' routing, so ends in different components have no path, and neither have the stricter searches. The
graph ignores the bend limit and the spacing between a net's own vias, so it joins at least what the search
reaches. *Pocket test* (searches that may not share, as in legalization and retries): everything from which a
target can be entered is visited backwards from the targets, through the static maps and around other nets'
routing, up to 50 000 nodes; when that runs out without meeting a source, the targets are walled in. On
glasgow-revD1 the first iteration's failing searches were 38 of its 52 million node visits and legalization's
44 of 46 million; both are gone.

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

**Gridless search** (`router: grid | gridless | auto`, `rooms` and `expansion`; D39). A shape-based search over
the free space of each layer instead of the grid:

- *Rooms.* For the net being routed, every obstacle its centerline must keep away from (other nets' pads and
  copper, keep-outs, NPTH holes, the board edge; for exact searches also the committed routing of every other
  net) is grown into its clearance region: the shape widened by the required distance plus 0.5 µm, as outer
  polygons (convex hulls widened by an octagon whose inradius is that distance; non-convex shapes per triangle, so
  an L-shaped keep-out keeps its notch). The board within a search region minus their union is computed exactly
  on integers by `polyclip`; a centerline anywhere in it keeps every clearance. A vertical (trapezoidal)
  decomposition of it (`polyclip::trapezoids`), with trapezoids larger than six grid pitches cut into a lattice of
  smaller ones so the search can choose its way through open areas, gives the *rooms*: convex pieces, joined by
  *portals* where two touch along a segment. The net's own copper is not an obstacle, so its pads and tracks lie
  in rooms. Via sites come from the same decomposition of the via-center free space (through vias: every layer,
  hole-to-hole to every drilled hole, no via in a pad): the center of each piece (pieces at most two via pitches
  wide), on every layer whose rooms hold it. Free-space cell decomposition for path planning (Chazelle 1987; de
  Berg et al., *Computational Geometry*, ch. 6 and 13), as "expansion rooms" of shape-based routers.
- *Search.* A* over points on the portals (one to three per portal), the via sites and the connection's ends
  (pad centers and points inside large pads, points along the net's tracks and earlier wires, so a connection
  can join a tree anywhere). Two points are joined when they lie in one room: every edge is a straight segment
  inside a convex room, legal by construction. Costs follow the grid search: length in grid pitches, 1 along the
  layer's preferred direction up to `wrong_way` across it, bends by angle (`bend45` at 45°, `bend90` at 90°),
  `via` per via; in negotiation the PathFinder costs of the occupancy maps are sampled along each edge every half
  pitch (present congestion and history, the maps the grid search uses), lazily: an edge is first priced by its
  length and sampled only when its end comes off the queue; negotiation searches weight the heuristic by 1.5
  (they cover whole net regions), exact searches are local and keep it at 1.
- *Path.* The node sequence fixes a channel of rooms per layer; the funnel algorithm (string pulling over the
  portals, as in navigation meshes; Lee and Preparata 1984) gives the shortest path through that channel, which
  stays inside it. Segments that are not 0°/45°/90° are replaced by the octilinear two-segment path when that is
  legal (exact checks), unless `any_angle`; the result is checked exactly (segments, vias, hole-to-hole with the
  net's other vias) before it is kept.
- *Negotiation.* A gridless wire is a polyline (slot per vertex). It claims its halo on the occupancy maps
  like an off-grid stub (widened by the sampling margin), so grid wires negotiate with it; its own conflicts are
  computed exactly against the committed routing in the engine's index (all wires of all nets), at the nearest
  map point for history. Negotiation searches rooms built from the static obstacles only, kept per net between
  iterations (region: the net's terminals plus margin); exact searches (`Mode::Hard`: legalization, retries) build
  rooms that also keep away from every other net's committed routing, in a region around the connection (its ends
  plus 2 mm and half its size, then the net's region), so what they find is legal as found.
- *`gridless`* routes everything with this search (negotiation, legalization, retries, via minimization);
  *`grid`* is the grid router alone; *`auto`* (the default) runs the grid router (negotiation, legalization,
  retries) and then gives every connection it left unrouted (no grid path, no grid access, or no room) an exact
  gridless search, then a *gridless rip-up and retry*: a probe search for the failed connection that keeps every static rule but may run through other nets' routing (present cost ×4) names
  the nets in its way (exact checks); with at most 8 of them, they are ripped, the probe becomes the connection's
  wire and they are routed again (grid, then gridless for their own failures); kept when fewer connections fail
  (or, four times, when as many fail but this one is routed), else everything goes back. Running the gridless
  search inside negotiation for what the grid cannot reach was tried and dropped: it slowed the large corpus
  boards' iterations (6-layer glasgow-revD1 fell from 92.8 % to 74.3 % in the 60 s budget) for no completion gain.

**Neck-down** (`neck`, on by default). A net whose width cannot leave one of its pads (no straight exit from the
pad center out of the pad, in any of the eight directions on any routing layer, is legal against the other nets'
pads and the static obstacles; typical of 0.4 mm QFNs and fine-pitch connectors with a 0.25 mm class width) is
routed at the widest width that gets every such pad out, in 5 µm steps, not under the board's `min_track_width`,
and at most the pad pitch minus the clearance so that two such tracks fit side by side out of neighboring pads
(less the grid's sampling margin and the router's tolerances, so that two escapes ending on cells beside each
other's tracks still clear them as the grid samples them). A net whose class width does leave its pad is necked
the same way when the pad is a single-layer pad of a fine-pitch row and two tracks of that width cannot run side
by side out of the row at the pad pitch with the clearance outside the part (D64): on tinytapeout-demo the 0.22 mm
Default class out of the 0.4 mm QFN-80 got out of its pads under the courtyard rule (0.16 mm) but walled in every
other pad beyond it (0.2 mm), and now routes at 0.185 mm there.
The net gets a profile of its own (its class profile with that width) for the whole run, so fanout, search and
negotiation all work at it; at the end every straight segment of its new tracks is widened back to the class width
wherever that keeps the clearance to everything else (exact checks, in route order). What stays narrower than the
class shows as a `drc.track_width_class` warning; the board's `min_track_width` is never crossed. KiCad's net class
widths are defaults, not rules, and designs at this pitch route them narrower the same way. Escapes are
still planned for such parts (at the neck width): leaving their pads to the gridless search alone routed a 0.4 mm
QFN-40 better (85 % instead of 75 %) but lost on the corpus boards (corne-cherry 92.8 % → 89.9 %,
tinytapeout-demo 78.9 % → 72.0 %), where the escapes guide the grid out of the pad rows.

**Small dog-bone vias.** When the net's via does not fit at a dog-bone site between four balls (0.6 mm vias under a
0.8 mm-pitch BGA), the fanout tries the smallest via the rules allow: the minimum drill with the minimum annular
ring (`min_drill`, `min_annular_ring`), checked exactly like any other (`drc.via_size_class` warns that it is
smaller than the class via).

**Arc corners** (`arcs`, off by default; `arc_radius`, default 1 mm). The last step: every bend that is not a via,
junction or terminal becomes a circular arc tangent to both segments (stored as the track's `mid`). At a corner
with interior angle φ an arc of radius r touches the segments at r / tan(φ/2) from the corner; it may use what the
previous arc left of the first segment and half of the next one (all of it when it ends at a pinned vertex). The
radius starts at the largest that fits, capped by `arc_radius`, and halves until the arc is legal, down to the
track width: the arc is checked as chords at most 0.1 µm from it with 0.1 µm more clearance, and inserted into the
index as those chords for later nets. Gerber (G02/G03), KiCad (`arc`) and IPC-2581 export arcs as arcs; the DSN
export writes them as chords within 1 µm. Lengths (`route.status`, stats, benchmark) are measured along arcs
(`board::track_length`).

**Verification.** `drc::check_copper`, the checks of `drc::check` that can flag a track or via (the same errors about
them, D72), runs on the project with the result; any new track or via in an error has its wire ripped and reported. The final status of every connection is read from the copper
islands of the verified board.

**Finer grids** (D84; `route.all`, `route.nets`). The default pitch holds the paths that lie on its lines. Between
parts placed for hand routing the passages are often narrower than a pitch and off those lines, and negotiation
then never settles: nrfmicro (85 connections, 0.2 mm tracks at 0.2 mm) still shares 20 to 40 points after 25 to 40
iterations at 0.2 mm, and legalization leaves 2 to 3 connections whatever the seed. A whole run (with the re-plan
of boxed pads and the push-and-shove of leftovers) that leaves a few connections unrouted (at most 4, or a tenth)
without having ended on its budget is made again at a pitch of 1/1.5, 1/2 and 1/3 of its own, then the same three
with the next seed, until one routes everything; the run with the fewest failed connections is kept, the earlier
among equals. Only when the run left a connection with other nets' routing in its way ("no room left": a keep-out
across the board or a pad no track leaves fails on any grid, and the exact gridless search has been there
already), or with `router: grid`, which has no other way off its lines. The first of them needs four times what
the first run took left in the budget, each further one twice what the costliest so far took, so a board that
ends near its budget gets none, and neither does one with an explicit `grid`. Which pitch settles is chaotic (of
seeds 1 to 8, a pitch of 0.133 mm alone routes nrfmicro completely for 2 and 0.1 mm for 6, not the same ones),
hence several: nrfmicro routes to 100 % for seeds 1 to 12, in 15 to 99 M units of work (2 to 12 s on the loaded
development machine) where one run took 8 to 28 M. *The cost:* a board with a few connections that congestion
really leaves no room for now spends the rest of its budget on runs that change nothing. The 0.4 mm QFN-40 of
`tests/route.rs` (39 of 40 routed) takes 44 s of its 60 s budget in a release build instead of a few seconds, for the
same 39; a shorter `budget` is the way to a quick answer.

**Failure reports.** A failed connection is re-searched with obstacles passable at a penalty; what that path runs
through (exact checks) names the blockers: `no path from C2.1 to U1.5: blocked by keep-out `wall` on F.Cu`, with a
location, layer, subjects and hints (move a component, rip the routing in the way, allow more layers, lower the
clearance, raise the budget).

**Determinism and control.** No hash iteration, ties broken by node index, seeded RNG; progress per net and
iteration through a callback, cancellation polled every 4096 search expansions, and a budget (negotiation stops at
75 %, the other searching phases at 100 %, of what is left once the estimated end of the run is set aside) that returns
the best legal partial result. Optimization, verification and reports always run; they get about 12 000 units of work
per connection out of the budget, in time at the speed the run's setup showed (D72).

**Work budget** (`work_budget` on `route.all`, `route.nets`, `route.connection`, `route.diffpair`; `RouteOptions::work`
in the library; `router::work`, D72). A time budget that runs out makes the result depend on the machine and its load.
The work budget ends the same phases at a count of the run's own operations: one unit per pop of the grid search's
queue, the weights of the other operations (gridless nodes and rooms, pair search, halos, setup, post-processing and
verification by size) measured to take about the same time per unit, 200–300 ns per unit on the development machine on
the large corpus boards. Searches of a parallel batch charge a pending count that joins the total when the batch ends,
so stop tests never see another thread's progress; a batch can overshoot by its own size. Results are identical on any
machine, under any load and with any number of threads (seen: 1, 3, 4, 8, 64 threads, a machine at load 100 and a CI
runner gave the same tracks, vias and work). `stats.work` reports the work of every run: a scale for choosing a budget, not a
way to repeat a run the clock cut (the phases end at shares of the budget, so another budget is another run).
What a user gets:

- *Default: a time budget (60 s).* A board that finishes inside it is deterministic anyway (all of the generated
  benchmark, 8 of the 12 corpus boards). One that does not gets the best result the machine could reach in that time,
  which differs from run to run; `route.budget_cut` (info) says so and gives the run's work.
- *`work_budget`:* the same result every time, the choice for scripts, regression checks, or anything whose output is
  checked in. The time is what varies, with no bound but the count: revD1's 200 M units took 43 s and 70 s in two CI runs of one
  commit, with identical results, and up to 170 s on the loaded development machine. Giving both budgets is refused (`route.budget`), since the clock
  could then decide. The default stays the time budget because a time limit is what an agent or a person waiting for an
  answer asks for, and a unit is not a fixed time: 200 M units are 33 s on tinytapeout-demo and 44 s on glasgow-revD1, so
  no count means "a minute" on every board.
- A unit measures the algorithm, not the code: a faster build does the same work in less time and routes the same,
  so `stats.work` shows search savings, and the benchmark's time column shows speedups per unit.

**Commands.** `route.all {budget? ("60s") or budget_ms? or work_budget?, layers?, effort?, seed?, fanout?, any_angle?, gridless?, arcs?,
arc_radius?, router?, neck?}`, `route.nets {nets, ...}` (net or class names, `class:power`), `route.connection {from, to, shove?,
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
`route.nets` (`shove`, on by default, D62) then gives what is still unrouted `route.connection`'s whole
treatment, connection by connection within what is left of the budget: push-and-shove again, then the rip-up
and reroute fallback (session 4: a GND connection `route.nets` failed and `route.connection` routed). This is
the command layer calling the router once per leftover (`Scope::Connection` with `shove`), so the router's
internals are unchanged; `route.all` does not do it (benchmarks keep their numbers). A connection a run leaves
failed whose ends are pads gets a first hint naming the command that can move other nets' routing
(`` `route.connection U1.3 C2.1` ``) wherever that was not tried yet (`route.all`, `route.nets` with `shove:
false`), ahead of the router's own hints (move parts, smaller clearance).
The result lists the nets moved (`rerouted`) and replaces their old copper.

### Keep-out net exceptions (D76)

A keep-out's `allow_nets` (docs/BOARD.md, "Keep-outs and net exceptions") makes it an obstacle that does not
exist for those nets: `Obstacle::except` holds their net ids and `Obstacle::spares(net)` answers. Every place that
asks whether an obstacle is in a net's way asks it too: the exact checks (`Checker::seg`, `Checker::via`, used by
the fanout, the optimizer, the arcs, length tuning and the failure explanation), the free space of the gridless
rooms (`rooms::track_space`, `via_space`), the hulls of the gridless refinement, and the push-and-shove world
(`seg_hits`, `via_hits`), which is how `route.track` refuses another net's track through the keep-out and never
pushes a neighbour into it.

The grid's static maps are per rule profile, not per net, so a point's state says *which* nets may use it: free,
one net (`net + 1`: near that net's own copper, or inside a keep-out excepting it alone), a set of nets (`SET + i`,
an index into `Statics::sets`: inside a keep-out excepting several), or blocked. A point within reach of several
obstacles gets the nets every one of them lets through (`Sets::combine`: the intersection, a single net falling
back to its one-net state, none to blocked), so a pad of another net inside the keep-out still blocks, and two
overlapping keep-outs let through only the nets both except. `Statics::ok(state, net)` is the one test the search
uses; the reachability components (`components`) add a set's points to each of its nets.

A differential pair is searched as a centerline with a net of its own (`~pair:<name>`): it is added to the
exceptions of the keep-outs that except *both* nets of the pair, so a pair never enters a keep-out one of its
nets may not.

A connection that fails at a keep-out names it (`blocked by keep-out `wall` on F.Cu`) and hints at the exception:
`change keep-out `wall`, or except net GND from it (keepout.set --allow-nets)`.

### Differential pairs (`pairs`, M8)

A pair is a circuit-level definition (`diffpair.add P N [--name] [--class] [--max-skew] [--max-uncoupled]
[--from] [--to] [--max-stub]`, changed by name with `diffpair.set NAME`, stored in `circuit.json` under
`diffpairs`); `diffpair.suggest` proposes pairs from net names (`X_P`/`X_N`,
`X+`/`X-`, `XDP`/`XDM` or `XDN`, `XP`/`XN`; nets already paired are skipped), `diffpair.list` shows each pair
with its rules and measured lengths, skew and coupling. Width and gap come from the pair's net class (its
`class`, else the positive net's): `diff_pair_width` and `diff_pair_gap`, which `impedance.solve --gap
--netclass` writes for a differential impedance target (ELECTRICAL.md); without them the class track width
and clearance are used (`diffpair.no_class_rules` note). Nets of a pair use the pair width everywhere (the
router's profile, `track.add`'s default and the DRC's class width check), and the DRC allows the pair's gap
between its two nets where it is smaller than the clearance.

`route.diffpair {pairs?, layers?, budget?, skew?}` (`router::route_diffpairs`):

1. **Plan.** Each pad of the positive net is matched with the nearest free pad of the negative net (greedy
   by distance, at most 10 mm apart): an *end*. Ends are joined by a minimum spanning tree over their
   midpoints (Kruskal; an edge is taken only when it joins new islands of both nets), so both nets get the same
   topology. Ends on one component (an ESD array whose lines pass through it, pins 1/6 and 3/4) are joined
   first by a straight *flow-through* link per net, and such a part sits in the signal path: its two sides join
   the ends of two other parts (the pair of links shortest together), not one side hanging off the part beside
   it on a branch because the receptacle is closer to the MCU than to the array's far side. When a connection then
   fails because the nets would swap sides (polarity), the outside ends join the other sides (the pair goes
   through the part the other way) and the pair is routed again; the result with fewer failures is kept.
   Pads without a partner (a pull-up on one net) are left to
   the ordinary router and counted in the report (`left`, `route.diffpair_left` note), and so are
   *interleaved* contacts of one part, whose straight link is blocked by a pad of the other net (a USB-C
   receptacle's two rows, D− D+ D− D+): the ordinary router joins them with a via or a loop (D53).
2. **Search.** Each coupled connection is routed as one fat path: the centerline, by A* on a grid (pitch a
   quarter of the fat width plus clearance, at least half the pair pitch) over states (cell, direction,
   polarity) with 0°/45° moves only (45° bends cost 0.6 steps; no 90° bends, no vias: the pair stays on one
   layer, the one its impedance was solved for). A move is legal when a capsule of radius `gap/2 + width`
   grown by the outer-corner reach of a 45° bend (`(width + gap)/2 · (1/cos 22.5° − 1)`) keeps the clearance:
   exact checks against the obstacle index, cached per cell and direction. The pair's own pads are obstacles
   for the centerline, so it starts clear of them.
3. **Breakout.** Sources and targets are the grid states near each end (within 1.2 × the pads' distance plus
   the pair width, at least 1.5 mm) from which straight stubs from both pads to the two offset points are
   legal: exact clearance checks, the stubs keep the gap from each other and from the other track's first
   step, and never double back (interior angle at the junction ≥ 90°). They are priced at twice their length
   (uncoupled track is dearer than coupled), so breakouts stay short. The polarity (positive net left or right
   of the centerline) is part of the state, fixed at the source; when no target fits but the opposite
   polarity would, the failure says the nets swap sides between the ends (`polarity: ...`, with hints: rotate
   a part, swap pins; a pad row tapped from one side reverses a pair).
4. **Split.** The centerline is offset by `±(width + gap)/2` with mitered corners (45° turns only, and each
   grid step is at least the miter's length, so offsets never invert); the stubs join the pad centers. Both
   tracks are checked exactly (clearance to everything, the gap between them), inserted in the index for the
   next connection, and the result goes through the DRC: a connection with a flagged track is taken out and
   reported.
5. **Skew** (`skew`, default on): see length tuning below; kept only when DRC-clean.

The report per pair (`PairReport`): status (`routed`, `partial`, `failed`, `nothing`), width, gap, layers,
lengths of both nets, skew, coupled length, largest uncoupled length, skew bumps, and per failed connection a
reason, location and hints (`route.diffpair_failed`); `route.diffpair_skew` / `route.diffpair_uncoupled`
warn when the pair's limits are exceeded. `route.all` (and `route.nets` naming both nets of a pair) routes the
pairs first, then everything else around them (`pairs: false` turns this off); push-and-shove never moves
pair copper, so the coupling stays. Coupled connections that failed are then routed by the ordinary router,
uncoupled (the pair report and the DRC show it).

### Length tuning (`tune`, M8)

Length groups (`lengthgroup.set NAME MEMBERS... [--target] [--tolerance]`, `circuit.json` `length_groups`):
nets and pairs (a pair counts as the mean of its two nets) that must be within `tolerance` (default 0.1 mm) of
`target`, or without a target within `tolerance` below the longest member. Lengths (`crate::lengths`) are
track lengths along arcs plus, for each via, the distance between the centers of the outermost copper layers
where the net's tracks meet it, from the stackup (dielectrics in effect, as the impedance calculator assumes
them; ELECTRICAL.md).

**Signal paths** (`crate::lengths::path`, D55). A pair's and a length group member's length is measured
along its signal path, not over all of its copper: a net's tracks, vias and pads form a graph (track ends and
T-junctions are nodes per layer, tracks edges of their length, a via joins its layers with edges of their
distance through the stackup, a pad joins the track ends and vias touching its copper), and the path is the
shortest one between the pads of its two end components. Ends are given (`diffpair.set NAME --from J1 --to
U3`, `lengthgroup.set ... --from --to` for every member; a pair's own ends win, `auto` clears them; they
follow `circuit.rename` and are dropped by `circuit.remove`) or chosen: for a pair, among the components with
pads on both nets (connector, ESD array, transceiver; a pull-up has a pad on one net only), the two farthest
apart along the routing (straight-line when none are connected yet); for a net, among all its components (its
pads when it has one). With several pads of an end component on the net (a USB-C receptacle's two rows), the
path starts at the nearest one along the routing. Everything off the path is grouped into **stubs** (a
pull-up's branch, the receptacle's row link), each with its length (tracks and via spans), where it leaves
the path and the pads it reaches; `diffpair.list` shows both nets' paths and stubs. When the ends are not
connected yet, the length falls back to all of the net's copper. Skew, the coupled and uncoupled lengths
(coupling along the path only), group members' lengths, `route.tune` (meanders and skew bumps go on path
tracks only) and the DRC all use the signal path, so a stub is neither skew nor tuned away.

`route.tune {group?, style?, amplitude?, spacing?, corner?, arcs?, skew?}` (`router::tune`):

- **Meanders** replace parts of a member's straight tracks, longest first: *trombone* (U bumps on one side;
  the side that worked last is tried first), *accordion* (U bumps alternating sides) or *sawtooth* (triangular
  teeth). Legs are `spacing` apart (default 4 track widths, at least width + clearance), at most `amplitude`
  high (default 1 mm, at least 2.5 spacings), corners 45° chamfers of `corner` (default spacing/4) or, with
  `arcs`, tangent arcs of that radius. Each bump is checked exactly against other nets (clearance, arcs as
  chords with 0.1 µm margin) and the net's own other copper (clearance as spacing); a bump that does not fit
  is halved until it does or becomes smaller than the track. The last bump's height is solved from the closed
  form of what a bump adds (U bump: `2h − (8 − 4√2)c` chamfered, `2h − (8 − 2π)r` with arcs; tooth:
  `2√((s/2)² + h²) − s`), so the target is met to within vertex rounding (nanometers).
- **Pairs** are tuned as pairs: bumps on the centerline of a coupled straight stretch (both tracks parallel at
  the gap), offset to both tracks (concentric arcs, mitered chamfers). A U bump turns left and right equally
  often, so both nets gain the same length and the skew does not change; the legs are far enough apart that
  neighbouring legs are not taken as coupled (more than 1.5 gaps).
- **Skew compensation** (all pairs, or the pairs in the group): when the two nets differ by more than 2 µm, 45°
  triangular bumps (height at most max(width, gap), adding `(2√2 − 2)h` each) go on the shorter net, away from
  its partner, on the coupled stretches nearest the bends where it is the inner track (where it lost length),
  sliding along a stretch past places where they do not fit.
- Tuning only adds length: members above their range are reported (`route.tune_unmet`), as are members without
  room. Everything is DRC-checked at the end; a member (or a pair's skew bumps) whose new copper the DRC flags
  is put back as it was, with any later change on the same nets.

The output gives, per group, target and range, and per member the length before and after, the residual error
and the bumps added; per pair the skew before and after.

### DRC checks for pairs and groups

All warnings (the board is manufacturable either way); `crate::lengths::checks`, run by `drc.run`:

| Code | When |
|---|---|
| `drc.diffpair_gap` | a coupled section's edge-to-edge gap differs from the pair's by more than 10 % (at least 5 µm), worst per layer |
| `drc.diffpair_width` | a track in a coupled section is not the pair width |
| `drc.diffpair_uncoupled` | a net's track length outside coupled sections exceeds `max_uncoupled` |
| `drc.diffpair_skew` | the signal paths' lengths (vias included) differ by more than `max_skew` (fully routed pairs only) |
| `drc.length_mismatch` | a fully routed group member is outside its range, with how much too short or long |
| `drc.diffpair_stub` | a branch off a fully routed pair net's signal path is longer than `max_stub` (pair's, else its group's, default 5 mm), with the pads it reaches |
| `drc.length_stub` | the same for a net member of a length group (the group's `max_stub`) |
| `drc.diffpair_invalid` | a pair names a net that does not exist |

Coupled sections are pairs of parallel segments (within about 0.6°) of the two nets on one layer whose edges
are at most 1.5 gaps apart, measured over their overlap; arcs are measured as chords of at most 5°, so
concentric arcs of a pair give parallel chords.

### Results (pairs and tuning)

`tests/diffpair.rs`: a USB 2.0 device on four layers (0.2 mm prepreg): USB-C receptacle → flow-through ESD
array (SOT-23-6) → TSSOP-20 MCU, class `usb` solved for 90 Ω at 0.15 mm gap (0.259 mm wide). `route.diffpair`
routes the pair as three connections (coupled J1 → U2, flow-through U2, coupled U2 → U1), every coupled section
at 0.15 mm ± 2 nm, skew compensated to 0 with three small bumps, DRC-clean, in well under a second in a debug
build; `route.all` routes the pair first and completes the board around it (after the ESD's GND and VBUS vias
are placed by hand: those pins sit between the flow-through lines). Eight DQ nets of 20–31 mm are matched to
the longest within 0.1 mm with trombones, accordion meanders with arcs and sawtooth teeth, and to an absolute
33 mm ± 0.05 mm, DRC-clean; the USB pair in a length group is lengthened 3 mm with coupled meanders, keeping
its gap and skew. Results are identical for 1 and 4 threads.

### v1 limits

- Through vias only; no blind/buried vias, no via-in-pad (dog bones only).
- Neck-down is per net: a necked net routes at its neck width everywhere and is widened afterwards where the
  class width fits, so a long run between two fine-pitch parts can stay narrow. Only dog bones get smaller vias.
- Scoped rules: near a boundary (within the copper's reach, or a grid move's span) the larger of the clearances
  on both sides applies; the grid pitch follows the net classes, not the rules; custom track width rules bound
  neck-down widths only (a class width under a rule's width elsewhere is left to the DRC); copper moved by
  push-and-shove and gridless rooms take the largest clearance any copper near an obstacle could need. More than
  64 positional conditions are "maybe" everywhere.
- The gridless search is region-limited (its rooms cover the connection or the net with a margin, not the whole
  board) and its probe-based rip-up is limited to 8 nets per failed connection; `gridless` alone negotiates less
  well and much slower than the grid on grid-friendly boards (see the benchmarks), so `auto` is the default. Its
  via sites are the centers of the via free space's pieces, not every legal position. Arcs only as corner
  rounding (`arcs`).
- Push-and-shove moves straight unlocked tracks and vias; arc tracks, lines of mixed widths and closed loops stay
  fixed, vias are pushed as through vias, lines are not split or merged, and a free end (a track ending on
  nothing) cannot move. A shoved line keeps its layer.
- A track crossing a pour can split it. The run fills the pours for its routing and plans the nets it cut off
  again (Pours, above), within the budget and a few rounds; what is still cut off is reported as failed.
- Hole-to-hole between vias of the same net in *one* wire of a search is left to the final DRC.
- Differential pairs: one layer per coupled connection, no vias in a pair (no layer change, no polarity swap),
  0°/45° centerlines only (no arcs while routing; meanders may use arcs), straight breakout stubs. Pads tapped
  from one side reverse a pair (reported as a polarity failure): such parts need flow-through pins or a route
  past them with stubs. Pads enclosed by a flow-through pair (an ESD array's GND) need a via placed first.
- Tuning adds length only, on straight tracks of the signal path (arcs are never meandered), and does not
  reroute to make room. Copper reaching a net only through a zone is not part of its graph.
- Parallelism is per batch of nets with disjoint regions: compact boards, where most nets meet near the same
  parts, get little of it. Memory is a few bytes per double-grid point per layer and profile, plus one search
  scratch (33 bytes per node) per thread.

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
| `keepout` / `wire_keepout` / `via_keepout` | keep-outs forbidding tracks and vias / tracks / vias, one per layer (`signal` when all); pour- or footprint-only keep-outs are left out with a warning; net exceptions are not written (the external router keeps those nets out too, `export.keepout_net_exception`, D76) |
| `structure/via`, `structure/rule` | via padstacks in use (rules default first); rules track width and clearance |
| `placement` | each footprint: image = footprint name, position, `front`/`back`, rotation, `lock_type position` when locked, `PN` = part; mounting holes as components `H1`… (locked) |
| `library/image` | pins in footprint-local coordinates (pin ID = pad number; repeated or empty numbers become `<number>@<index>`), courtyard as outline, non-plated holes as image keep-outs; a non-plated mounting hole is an image with only a keep-out |
| `library/padstack` | one per distinct pad: `smd_…` on `F.Cu`, `tht_d<drill>_…` on every layer; rectangles as `rect` (a quarter turn swaps the sides), circles as `circle`, ovals as `path` with the minor width, round rectangles as polygons circumscribing the corner arcs (4 segments per corner), other pad angles as rotated polygons, polygon pads (imported KiCad custom pads), trapezoids and chamfered rectangles as `poly_<hash>` polygons (rounded corners within 1 µm outside); vias `via_<diameter>_<drill>[_<from>-<to>]` in µm |
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
  Specctra/freerouting-class routers), with search over room boundaries. *Done:* gridless refinement of grid
  routes (visibility graph over clearance hulls) and the gridless search over expansion rooms (`router`, above,
  D39).
- Exact DRC by construction, with no grid quantization errors, which matters for fine-pitch parts.
- **Push-and-shove**: route a new track by moving existing ones out of the way while keeping them valid. Needed
  for incremental routing via API ("add this one net without destroying the rest"). *Done:* `route.track`,
  `route.connection` and leftovers of `route.all` / `route.nets` (above).
- Fanout strategies for BGA/QFN (dog-bone, via-in-pad when allowed). *Done:* dog bones and escapes (D28).
- Diff pairs routed as coupled pairs. Length matching via meander insertion (with M8). *Done:* `route.diffpair`,
  `route.tune` (above, D38).

## guide as a backend (`router: guide`, D85)

[guide](https://crates.io/crates/guide) (MIT, the same owner as cadlab, a standalone placement and routing engine
in its own repository, as polyclip is for polygons) has a PCB router, `guide::pcb::route`. `router: guide` on
`route.all` and `route.nets` runs it in place of cadlab's own search, through `router::guide`, an adapter in the
library (no command, no file in between: the board goes to guide's `pcb::Design` and its tracks and vias come back
through guide's API). The default router is unchanged, and so is everything the native router does. The dependency
is `guide = { version = "0.0.2", default-features = false, features = ["pcb"] }`: its only dependency is polyclip
0.0.6, the version cadlab uses, so the tree has one polyclip and nothing new besides guide itself.

**What goes to guide** (`router::guide::adapt`):

| cadlab | guide |
|---|---|
| copper layers of the stackup | `RuleSpec::layers`, by position |
| nets (circuit, board copper, zones) | `NetId` by position in name order; `NetSpec { route, track_width, via }` from the net's class (the pair width for a pair's nets); only the nets with open connections in scope have `route` |
| rules | `RuleSpec`: `min_track_width`, `clearance`, `min_clearance`, `hole_clearance`, `hole_to_hole`, `min_drill`, `min_annular_ring`; `copper_to_edge` as `Design::edge_clearance` |
| net classes with a clearance | `NetClass` with that clearance override (guide resolves the larger of two nets', as the DRC does) |
| differential pair gap (`diff_pair_gap`) | `RuleSpec::pairs`: the smaller of the gap and the clearance between the two nets, as the DRC allows |
| pads | `drc::Pad` with the exact shape where the pad has one (`board::exact`: a circle, a segment or a polygon grown by a radius, which is guide's shape model too; chamfered round rectangles as their polygon), its layers, its hole (round or slot) and its local clearance |
| pad stacks (another shape on some layers) | one `Pad` per distinct shape on its layers, and the plated barrel as a fixed `Via` of the pad's net (1 µm ring) that carries the hole and joins them, since a guide pad has one shape on all its layers |
| unplated holes, mounting holes | a `Pad` with a hole and no copper layer when the rules set a `hole_clearance`; without one the DRC and guide only forbid copper over the hole while the native router keeps the copper clearance from it, so the hole is given as copper of no net on every layer and guide's routes keep that distance too (0.2 mm instead of 75 µm in `guide_keeps_clear_of_unplated_holes`) |
| no via in a pad (the native router's rule; not a DRC rule) | a `drc::Keepout` for vias over every pad with copper, on its layers. guide has no such switch and puts vias in pads on its own: 40 of the 148 vias of soic24-2l, 52 of 106 on fine-4l, 353 of 613 on cynthion (`RouteOptions::via_in_pad`, which no command offers, leaves it to do so, for measuring) |
| existing tracks (arcs with their middle) and vias (their layer span) | `drc::Track`, `drc::Via`: fixed copper, locked or not |
| copper drawings and copper text | fixed `drc::Zone` of their net, or `Design::obstacles` without one |
| keep-outs (`no_tracks`, `no_vias`, layers) | `drc::Keepout`; `no_pours` as `Zones::keepouts` |
| outline and cutouts | `Design::outline`: one edge per segment of each contour (arcs at the copper tolerance) |
| zones | `Design::zones`: outline, layers, net, priority and the effective fill settings (`zone_params`: clearance, minimum width, pad connection, thermal gap and spoke width), pad overrides as `Zones::pads` |
| differential pairs in scope (`pairs` not false) | `Design::pairs` at the pair's width and gap; a pair's `max_skew` as a `LengthConstraint` |
| length groups whose members are all routed | `Design::lengths`: `target ± tolerance`, or `max_skew = tolerance` without a target |

**What comes back.** guide's tracks (straight, octilinear) and through vias become unlocked tracks and vias of
their nets. Its zone fills are dropped: zone filling stays cadlab's (they are derived data here, and the DRC, the
exports and the count all read cadlab's fill). The result is then treated as the native router's:

- *Verification.* `drc::check_copper` on the board with the new copper; a connected piece of new copper (tracks
  sharing an end, vias at an end) holding a flagged item is taken out, a net's whole new copper after three
  rounds; `stats.drc_removed` counts the items. Nothing the DRC flags is ever applied.
- *Pour rounds.* guide plans a net with zones on its own fill of them, which is not cadlab's (thermal spokes, what
  counts as an island, the edge): a pad its fill joins can be open on the board as cadlab fills it. After the run
  the nets with zones that the board (filled by cadlab around the new copper) still has open go to guide again,
  on the routed board with cadlab's fills of those nets as fixed copper of theirs and no zone to fill; a round is
  kept when it leaves fewer connections open, up to three (`stats.iterations` is the number of calls of guide).
  Only nets guide took for routed: one it reported unrouted has had its searches, and a round is not a second
  attempt (corne-cherry has 77 such nets, and a round with them had not ended after 16 minutes).
  buspirate-flash-sop and buspirate-rs232 each have one pad (C1.2 of `GND_CHIP`, J2.5 of `GND`) that guide reports
  connected and cadlab's fill leaves open; one round joins it. A round is a whole run of guide, with nothing to
  bound it: seconds on those boards, 250 s on lumenpnp-mobo.
- *The count is the board's* (D80). Completion is read from the board with its pours filled again: failed
  connections are the ratsnest lines the board has left for the nets in scope, pads cut off from a pour included;
  routed ones are counted per island of the result (an island holding `k` of the planned islands stands for
  `k − 1` connections), because guide joins a net as a tree of its own, not along the planned lines.
- *Stats* in the native shape: `connections`, `routed`, `failed`, `completion`, `tracks`, `vias`, `length`,
  `grid` (guide's lattice pitch, half the smallest width plus clearance of the routed nets, or `grid`),
  `vias_removed` (guide's cleanup), `drc_removed`, `work` (guide's expanded search nodes: a count of its own, not
  cadlab's work units), `budget_exhausted` always false. Pairs are reported as the native router reports them,
  measured on the routed board (`lengths::coupling`).

**Which operations.**

| operation | with `router: guide` |
|---|---|
| `route.all` | routes every open connection |
| `route.nets` | routes the listed nets; other nets' copper is fixed. `shove` (the native push-and-shove of leftovers) does not run |
| `route.connection` | refused: `route.guide_unsupported` (subject `option:router`); guide routes whole nets and has no push-and-shove |
| `route.track`, `route.fanout`, `route.diffpair`, `route.tune`, `route.rip`, `route.status`, `route.import_ses` | take no `router`: always native. They work on a board guide routed as on any other |

**Options.** `pairs: false` routes a pair's nets uncoupled; `grid` sets guide's lattice pitch. `layers` and
`work_budget` are refused (`route.guide_unsupported`, subject `option:layers` / `option:work_budget`: guide cannot
be kept to some layers, and has no count to stop at). `seed`, `effort`, `fanout`, `neck`, `any_angle`, `gridless`,
`arcs`, `arc_radius` and `shove` have no equivalent and are named in one warning, `route.guide_option_ignored`.
The time budget cannot be kept: guide's PCB router has no time limit, no cancellation and no progress callback
(0.0.2), so a run is one call that ends when guide ends it. Cancellation is honoured before and after that call
(nothing is applied), progress says `guide: routing`, then what guide reports of the run, then `verifying`, and a
run that took longer than its budget says so (`route.guide_no_budget`, info). guide uses one thread and no seed:
the same board gives the same result on every run (`guide_is_identical_for_any_thread_count`).

**What guide cannot express**, passed the stricter way and reported (warnings with subjects and hints):

- `route.guide_keepout_exception`: a keep-out's `allow_nets` (D76). The keep-out is kept for every net, as for
  the exchange formats without an exception, so the excepted nets are not routed through it.
- `route.guide_custom_rules`: custom clearance rules and `disallow` rules by area, courtyard, layer or kind (D61).
  guide gets the class and pad clearances only; copper that breaks a custom rule is taken out by the verification.
- `route.guide_net_tie`: net ties. Their copper is fixed copper of one net, so the other tied net may be shut out
  of its pad.

Not passed at all: neck-down (a net has one width), smaller vias under BGAs (a net has one via), layer
restrictions, a zone's number of thermal spokes and its spoke angle (guide's reliefs have four spokes, in the
pad's own direction).
A backend error (geometry guide rejects, or a panic, which is caught) is `route.guide_failed` and leaves the board
unchanged.

## API surface

| Command | Purpose |
|---|---|
| `route.all` | route all unrouted connections (`router: grid`, `gridless`, `auto` or `guide`) |
| `route.nets` | route specific nets or net classes (the same choice of `router`) |
| `route.connection` | route between two pads, pushing unlocked copper of other nets aside (`shove`) |
| `route.track` | place a track along waypoints with push-and-shove (walkaround, shove, spring-back) |
| `route.fanout` | fanout a component: BGA dog bones, fine-pitch escapes |
| `route.optimize` | post-process existing copper only (to come) |
| `route.rip` | remove routing (net, area, all non-locked) |
| `route.status` | unrouted connections, completion %, problem areas |
| `route.diffpair` | route differential pairs coupled at their width and gap, then compensate skew |
| `route.tune` | meanders to bring length groups to target, pair skew compensation |

Options: time budget, max iterations, seed, layer restrictions per net class, via cost, allowed angles,
keep/rip existing copper, effort level. `router: guide` runs the guide crate's router instead of cadlab's and
takes few of them ("guide as a backend", above).

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
scratch with `route.all {seed: 1, budget: "120s"}`:

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
`CADLAB_ROUTE_RENDER=<dir>` renders cadlab's results; `CADLAB_BENCH_ROUTER=grid|gridless|auto` picks the search,
`CADLAB_BENCH_ROUTER=guide` the guide backend (here and in the corpus mode; it takes no seed and no budget, so a
run is bounded from outside, one board per process under `timeout`), and `CADLAB_BENCH_SEED=<n>` the seed.
`route_bench --corpus [names...]` re-routes the open-source corpus instead (below); there `CADLAB_BENCH_TRACE=1`
prints a timeline of the router's phases and negotiation iterations (time and work), `CADLAB_BENCH_NETS=a,b` routes only
those nets, `CADLAB_BENCH_ORIGINAL=1` lists the connections each project's own routing leaves open instead of
routing (the reference for residuals), `CADLAB_BENCH_WORK=<units>` uses a work budget (repeatable: compare builds this way, not on time; the job
`corpus-route` of CI does it for three seeds) and `CADLAB_BENCH_SEED=<n>` sets the seed. For one place on a board,
`CADLAB_BENCH_DUMP=<refdes>[.<pad>][,<margin mm>]` lists the pads, tracks and vias of the result around a component
or pad (coordinates to compare with the original board's copper), and `CADLAB_BENCH_FANOUT=only` plans the fanout
alone and lists that.

**Results** (release build, 16-core macOS development machine, freerouting 2.4.1 on Java 25). *M5* is the router
at the start of M6, *phase 1* the first M6 version (fanout, batches, rip-up and retry), *phase 2* the second (escape
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

**Phase 3: the gridless search** (D39; same machine, `CADLAB_BENCH_ROUTER` picks the search). `grid` reproduces
phase 2 exactly (neck-downs and small dog-bone vias never trigger on these boards), and so does `auto`, the default,
except on stm32-4l, where the connection phase 2 left to push-and-shove is routed by the gridless pass instead
(2 vias fewer, 0.4 % more track). `gridless` alone:

| board | router | connections | completion | vias | length | segments | sharp corners | time |
|---|---|---|---|---|---|---|---|---|
| attiny-2l | gridless | 22 | 100% | 2 | 113.2 mm | 60 | 2 | 125 ms |
| soic24-2l | gridless | 184 | 100% | 95 | 1990.5 mm | 682 | 0 | 2.8 s |
| stm32-2l | gridless | 109 | 97.2% | 48 | 957.9 mm | 497 | 3 | 20.8 s |
| stm32-4l | gridless | 109 | 99.1% | 51 | 923.6 mm | 468 | 5 | 31.3 s |
| qfp-qfn-4l | gridless | 155 | 100% | 134 | 2137.3 mm | 879 | 4 | 105.8 s |
| fine-4l | gridless | 100 | 99.0% | 99 | 893.3 mm | 536 | 3 | 27.5 s |
| bga144-4l | gridless | 141 | 78.7% | 116 | 1524.9 mm | 726 | 3 | 40.6 s |

On these generated boards, whose pads sit on or near the grid by construction, the gridless search alone routes
less completely (most clearly bga144-4l, where it negotiates the escapes between the balls worse) and one to two
orders of magnitude slower: its negotiation searches whole net regions over thousands of rooms with lazily
sampled congestion, where the grid's A* steps are a few array reads. It uses 16–50 % fewer vias on the 2-layer
boards, with more track and segments. This is why it is an option and `auto` the default.

**Corpus re-route** (`cargo run --release --example route_bench -- --corpus`, D35's open-source boards; every
track and via ripped, zones kept, routed again with the default options, 60 s budget, release build, same machine;
*before* is phase 2 as measured by `tests/corpus.rs` at the start of phase 3, *after* this version). DRC errors
other than unrouted connections: 0 everywhere, before and after.

| board | layers | connections | before | after | main change |
|---|---|---|---|---|---|
| nrfmicro | 2 | 85 | 96.5% | 97.6% | gridless pass and retry (the USB-C D+/D− cross-over between the receptacle's two pad rows is left) |
| buspirate-flash-sop | 2 | 47 | 100% | 100% | |
| buspirate-rs232 | 2 | 43 | 100% | 100% | |
| buspirate5-rev10 | 4 | 480 | 99.6% | 99.6% | |
| lumenpnp-ringlight | 2 | 32 | 100% | 100% | |
| lumenpnp-mobo | 4 | 490 | 98.0% | 99.2% | gridless pass and retry |
| sweep-v2.2 | 2 | 43 | 93.0% | 93.0% | the 3 failures join the two halves of the split keyboard (also unrouted in the original) |
| corne-cherry | 2 | 435 → 431 | 69.4% | **91.9%** | neck-down out of the two RP2040s (QFN-56, 0.4 mm, 0.25 mm class width), gridless retry |
| glasgow-revC3 | 4 | 741 → 740 | 88.8% | **97.0%** | small dog-bone vias under the iCE40 BGA (0.6 mm class via does not fit between 0.8 mm balls) |
| glasgow-revD1 | 6 | 1666 → 1660 | 92.4% | 94.5% | small dog-bone vias; budget-bound (the 60 s budget cuts negotiation short) |
| cynthion | 4 | 748 → 745 | 95.6% | 95.4% | budget-bound; 8 resistor-array pads admit no track at the board's minimum width |
| tinytapeout-demo | 4 | 404 → 399 | 57.2% | **78.7%** | neck-down out of the RP2040 and the fine-pitch connector J5; budget-bound |

Connections count pads − 1 per net after fanout, so escapes and dog bones that join two pads of a net lower it
slightly. What was left then, by cause: budget (glasgow-revD1, cynthion and tinytapeout-demo ended negotiation
with overuse left), QFN pads boxed in by their neighbors' escapes and routes (corne, tinytapeout), pads no
allowed width can leave (cynthion), the USB-C D+/D− crossover (nrfmicro) and connections that the design does
not route on the board (sweep).

**Corpus re-route, D57** (same command; *before* is the version before D57 and *after* this one, run back to back
on a shared 64-core Linux machine, so times are only indicative; DRC errors other than unrouted: 0 everywhere,
before and after):

| board | layers | connections | before | after | main change |
|---|---|---|---|---|---|
| nrfmicro | 2 | 85 | 97.6% | 97.6% | the D+/D− crossover routes (crossover vias); two other connections near the USB-C receptacle fail instead |
| buspirate-flash-sop | 2 | 47 | 100% | 100% | |
| buspirate-rs232 | 2 | 43 | 100% | 100% | |
| buspirate5-rev10 | 4 | 479 | 99.8% | 99.6% | one more congestion failure (negotiation differs from the first iteration) |
| lumenpnp-ringlight | 2 | 37 | 100% | 100% | |
| lumenpnp-mobo | 4 | 461 | 99.1% | 99.3% | |
| sweep-v2.2 | 2 | 43 | 93.0% | 93.0% | |
| corne-cherry | 2 | 428 → 431 | 91.8% | **99.1%** | grid snapped to the 0.4 mm QFN pitch (0.195 → 0.2 mm): one RP2040 now sits on the grid and needs no escapes, the other gets 18 of its 25 (before: 41 of 53 and 37 of 52, every other pad walled in); crossover vias on both USB-C receptacles; capped present factor |
| glasgow-revC3 | 4 | 740 | 97.2% | 97.3% | |
| glasgow-revD1 | 6 | 1619 | 92.5% | **93.7%** | failing searches skipped (first iteration 12 → 6 s, legalization 10 → 2 s), more of the budget left to negotiation and retries |
| cynthion | 4 | 693 | 96.0% | 96.1% | negotiation converges no further (4–6 nets in conflict for its last 20 iterations); what fails is around the ECP5 BGA and 8 resistor-array pads |
| tinytapeout-demo | 4 | 359 → 351 | 81.1% | **83.2%** | no longer budget-bound (negotiation done at 16 s instead of 45 s) |

What was left then, by cause: the QFN-80 and the carrier connector J5 of tinytapeout-demo (45 of its 59 failures:
pads of the 0.25 mm-clearance Power/Analog classes are only legal under the board's custom rules scoped to the
QFN's courtyard, 0.16 mm for tracks, which the router did not apply), budget (glasgow-revD1 still ends negotiation
after about 9 iterations with overuse left; iterations are bound by the batches, about two nets each, so the
threads hardly help), cynthion's BGA and resistor arrays, congestion on the small nrfmicro around its USB-C
receptacle, and the split keyboard (sweep).

**Corpus re-route, D61** (scoped rules; same command, *before* the version before D61 and *after* this one, run
side by side on a shared, loaded 64-core Linux machine: boards that end on the budget move by a few connections
from run to run, and on tinytapeout-demo two runs of the same build gave 93.7 % and 95.3 %):

| board | layers | connections | before | after | DRC errors before → after | main change |
|---|---|---|---|---|---|---|
| nrfmicro | 2 | 85 | 97.6% | 97.6% | 0 → 0 | 6 vias fewer |
| buspirate-flash-sop | 2 | 47 | 100% | 100% | 0 → 0 | |
| buspirate-rs232 | 2 | 43 | 100% | 100% | 0 → 0 | |
| buspirate5-rev10 | 4 | 479 | 99.6% | **100%** | 0 → 0 | |
| lumenpnp-ringlight | 2 | 37 | 100% | 100% | 0 → 0 | |
| lumenpnp-mobo | 4 | 461 | 99.3% | 99.1% | 0 → 0 | one more congestion failure |
| sweep-v2.2 | 2 | 47 | 93.6% | 93.6% | 0 → 0 | |
| corne-cherry | 2 | 431 | 98.6% | 97.7% | 0 → 0 | budget-bound now (local clearances and hole clearance change the negotiation; with both ignored it routes exactly as before) |
| glasgow-revC3 | 4 | 740 → 739 | 93.0% | **94.9%** | **1 → 0** | hole clearance kept: before, 26–28 connections were ripped by the DRC (`drc.hole_clearance`, dog-bone vias at 0.2 mm from other nets' tracks, 0.25 mm asked) and one error stayed |
| glasgow-revD1 | 6 | 1619 → 1628 | 93.5% | 93.1% | **22 → 0** | hole clearance kept (22 fanout vias too close to other nets' pads before); budget-bound |
| cynthion | 4 | 693 | 96.1% | 96.0% | 0 → 0 | budget-bound |
| tinytapeout-demo | 4 | 351 → 364 | 83.2% | **93.7–95.3%** | 0 → 0 | the QFN-80's and J5's pads get out under the custom rules (0.16 mm in their courtyards, 0.18 mm for the QFN's pads): 124 escapes; 8–10 QFN pads are still walled in by their neighbors' escapes |

The ROUTER.md D57 numbers (corne-cherry 99.1 %, glasgow-revC3 97.3 %) came from an idle machine; on this one the
budget-bound boards end lower before and after alike. What is left: the QFN-80 pads without an escape on
tinytapeout-demo (greedy escape planning, as corne-cherry's before D57), budget (glasgow, cynthion, corne), the
split keyboard (sweep) and congestion near nrfmicro's receptacle.

**Corpus re-route, D64** (escapes per package, crowded-row necks, re-planned boxed pads, exact stub conflicts, costs
near the targets; same command, *before* the version before D64 (four runs of the budget-bound boards, two of the
others) and *after* this one (two runs), back to back on the shared 64-core Linux machine; ranges where runs
differed; DRC errors other than unrouted: 0 everywhere, before and after):

| board | layers | connections | before | after | time before → after | main change |
|---|---|---|---|---|---|---|
| nrfmicro | 2 | 85 | 97.6% | 97.6% | 1.2 → 1.2 s | |
| buspirate-flash-sop | 2 | 47 | 100% | 100% | 0.3 → 0.3 s | |
| buspirate-rs232 | 2 | 43 | 100% | 100% | 0.3 → 0.3 s | |
| buspirate5-rev10 | 4 | 473 | 100% | 100% | 24–26 → 9 s | costs near the targets; 7 vias fewer |
| lumenpnp-ringlight | 2 | 37 | 100% | 100% | 0.1 → 0.1 s | |
| lumenpnp-mobo | 4 | 461 | 99.1% | **99.3%** | 52 → 58–59 s | negotiation runs longer before converging |
| sweep-v2.2 | 2 | 47 | 93.6% | 93.6% | 28–29 → 29–30 s | |
| corne-cherry | 2 | 431 | 97.2–97.4% | **97.7%** | 60 s (budget) → 47 s | no longer budget-bound: negotiation searches take half the time; one more RP2040 pad escapes (27 of 28); 43–47 vias fewer |
| glasgow-revC3 | 4 | 739 | 94.9% | 94.9% | budget | |
| glasgow-revD1 | 6 | 1583 | 93.2–93.4% | **93.7%** | budget | searches ~10 % cheaper, more negotiation in the budget |
| cynthion | 4 | 693 | 96.0% | 96.0–96.1% | 63 s (budget in 3 of 4) → 60–61 s | negotiation converges inside the budget (searches at a third of the time) |
| tinytapeout-demo | 4 | 361 | 93.6% | **95.6–96.7%** | 62 → 61–62 s | QFN-80 exits 50 → 65 of 75 (assignment, 0.185 mm necks for the Default class); two pads left boxed in, re-planned when the time allows |

What is left: tinytapeout-demo's last boxed QFN-80 pads and congestion around the QFN and J5 (its gridless rip-up
and retry now takes 23 s, which leaves the re-plan too little time in some runs), the USB-C pads of corne-cherry
that no exit leaves (their receptacles' own pads wall them in), budget on the glasgow boards, cynthion's BGA and
resistor arrays, the split keyboard (sweep) and congestion near nrfmicro's receptacle.

**Corpus re-route, D69** (outer pads of USB-C crossovers, earlier-turning escapes, time for the re-plan of boxed
pads, partial rip-up of large nets near the overuse; same command, *before* the version before D69 and *after*
this one, back to back on the shared 64-core Linux machine with little other load; DRC errors other than
unrouted: 0 everywhere, before and after; boards not listed routed exactly as before):

| board | layers | connections | before | after | time before → after | main change |
|---|---|---|---|---|---|---|
| lumenpnp-mobo | 4 | 461 | 99.3% | 99.3% | 56 → 41 s | GND and +3V3 keep their wires away from the overuse |
| corne-cherry | 2 | 431 | 97.7% | **98.8%** | 44 → 31 s | the USB-C receptacles' CC1 and D+ pads get out (crossover via 0.1 mm deeper, escapes turning before the D− exits) |
| glasgow-revC3 | 4 | 739 | 94.9–95.1% | **95.4%** | budget | negotiation iterations 1.75 → 0.87 s: all 40 inside the budget instead of 24 |
| glasgow-revD1 | 6 | 1583 | 93.6–93.7% | 93.7% | budget | iterations 4.5 → 3.9 s (11 instead of 9 in the budget) |
| cynthion | 4 | 693 | 96.0% | 96.2% | 61 → 63 s | |
| tinytapeout-demo | 4 | 361 | 96.7% | **97.0%** | 62 → 61 s | the re-plan runs (the gridless phases leave it 10 s): no QFN-80 pad is left boxed in |

On the CI runners (`tests/corpus.rs`, 30 s budget, 4 cores, seed 0) the budget-bound boards move by up to ±1.5
points between two runs of the same build, more than the change; at 20 s on 2 threads, four seeds, tinytapeout-demo
failed 70 connections in total after and 81 before, and locally over seeds 1–3 (60 s) glasgow-revC3 102 after and
106 before, glasgow-revD1 301 and 311.

What is left: the shield-to-GND connection of corne-cherry's receptacle (a VBUS via in the way), budget on
glasgow-revD1, cynthion's BGA and resistor arrays (no exit for 0.127 mm tracks between their pads), the split
keyboard (sweep) and congestion near nrfmicro's receptacle (its D+ pads stay apart, blocked by the D− exits and
a no-connect pad).

**Corpus re-route, D72** (work budget; `CADLAB_BENCH_WORK=200000000 route_bench --corpus`, seeds 1/2/3, 3 per board since
negotiation is chaotic and a single seed says little: failed connections per seed. *Before*: the D69 searches, with only the
meter added, run locally, *after*: the CI job `corpus-route` on a 4-core runner; both repeat exactly. DRC errors other
than unrouted: 0 everywhere. nrfmicro, sweep-v2.2 and the small boards are not budget-bound.)

| board | layers | connections | before | after | main change |
|---|---|---|---|---|---|
| nrfmicro | 2 | 85 | 2/2/2 | 2/2/3 | crossover via deep enough for the partner's own exit where that exists; the D+/D− crossing still fails in most seeds |
| buspirate5-rev10 | 4 | 473 | 0/1/0 | 1/1/0 | |
| lumenpnp-mobo | 4 | 461 | 5/5/3 | **3/0/0** | via-aware estimate |
| corne-cherry | 2 | 431 | 8/6/11 | **5/4/7** | the shield "failures" were a report error: `J1.S1` names four pads, and the status looked at the first; they are routed |
| glasgow-revC3 | 4 | 739 | 35/35/34 | **34/34/31** | |
| glasgow-revD1 | 6 | 1583 | 130/115/125 | **115/112/110** | via-aware estimate; verification reserved |
| cynthion | 4 | 693 | 27/27/26 | 27/27/28 | 8 of them are the shunts' sense pads (below) |
| tinytapeout-demo | 4 | 361 | 21/11/12 | 20/19/19 | **worse**, from the via-aware estimate alone |

The other way to read completion: `route_bench`'s *DRC unrouted* column counts what `route.status` sees on the result, with the
pours filled again around the new copper, while `failed` counts the planned connections. They differ on boards with pours (revD1
115 and 199, corne-cherry 5 and 13, mobo 3 and 69): pads the plan took as joined by a pour are cut off from it by new tracks
(the limit noted under v1). So `completion` is optimistic on those boards, in every table above.

Times, on the wall-clock budget (60 s, seed 1, the development machine under other agents' load, indicative only): revD1 71–73 s
before and 63 s after, at 91.1–91.3 % and 93.1 %; its verification DRC 9.5 → 5.7 s.

What is left: cynthion's 8 sense pads lie *inside* the force pads of the shunts (`R_0603_1608Metric_Current_Sense`, net tie
groups "1, 2" and "3, 4"). No width gets out, and the original board's own tracks there are shorts for KiCad's DRC too: tried
first was exempting them (the oracle job rejected it, 11 `shorting_items` KiCad reports and cadlab then did not), so the
router reports them with a hint. nrfmicro's D+/D− crossing: congestion around the receptacle takes the room in most seeds. revD1
and revC3 are budget-bound at this count; corne-cherry's GND and QSPI leftovers near the RP2040s are congestion.

**Corpus re-route, D80** (pours; `CADLAB_BENCH_WORK=200000000 route_bench --corpus`, seeds 1/2/3, 4 threads, the shared
64-core Linux machine; *before* is the version before D80 and *after* this one, both repeat exactly, and after gives the same
tracks, vias and work on 1 thread: buspirate5-rev10, corne-cherry and lumenpnp-ringlight checked. *Open* is the number that
counts: the connections `route.status` and the DRC find missing on the routed board with its pours filled again. *Failed* is
what the run reports; before, it counted planned connections only. DRC errors other than unrouted: 0 on every board and
seed, before and after.)

| board | layers | connections (seed 1) | open before | open after | failed before | failed after | vias (seed 1) | length (seed 1) | work (seed 1) |
|---|---|---|---|---|---|---|---|---|---|
| nrfmicro | 2 | 85 | 2/2/3 | 2/2/3 | 2/2/3 | 2/2/3 | 37 | 447.8 mm | 7.9 M |
| buspirate-flash-sop | 2 | 47 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 4 | 460.2 mm | 1.6 M |
| buspirate-rs232 | 2 | 43 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 10 | 280.9 mm | 1.9 M |
| buspirate5-rev10 | 4 | 473 → 540 | 66/63/64 | **4/0/7** | 1/1/0 | 4/0/7 | 237 → 304 | 2891 → 3044 mm | 55.5 → 173.1 M |
| lumenpnp-ringlight | 2 | 37 → 39 | 2/2/2 | **0/0/0** | 0/0/0 | 0/0/0 | 0 → 2 | 214 → 180 mm | 0.7 → 1.4 M |
| lumenpnp-mobo | 4 | 461 → 521 | 69/62/61 | **8/14/14** | 3/0/0 | 10/16/17 | 230 → 341 | 5763 → 5518 mm | 200 → 204 M (budget) |
| sweep-v2.2 | 2 | 47 → 49 | 5/4/6 | 3/3/3 | 3/3/3 | 3/3/3 | 20 → 21 | 2137 → 2161 mm | 124.9 → 128.9 M |
| corne-cherry | 2 | 431 → 445 | 13/12/17 | **4/5/5** | 5/4/7 | 4/5/5 | 451 → 488 | 8405 → 8393 mm | 205 → 211 M (budget) |
| glasgow-revC3 | 4 | 739 | 32/33/30 | 35/32/34 | 34/34/31 | 41/37/41 | 400 → 422 | 3755 → 3398 mm | 211 → 210 M (budget) |
| glasgow-revD1 | 6 | 1583 → 1701 | 199/197/184 | **137/145/170** | 115/112/110 | 139/147/173 | 1000 → 1035 | 8950 → 8663 mm | 221 → 221 M (budget) |
| cynthion | 4 | 693 → 722 | 55/52/53 | **32/27/33** | 27/27/28 | 33/27/35 | 426 → 467 | 3118 → 3166 mm | 203 → 202 M (budget) |
| tinytapeout-demo | 4 | 361 → 393 | 37/41/37 | **29/23/28** | 20/19/19 | 29/23/29 | 250 → 265 | 4463 → 4384 mm | 200 → 200 M (budget) |
| all twelve | | | 480/468/457 | **254/251/297** | 210/202/201 | 265/260/313 | | | |

Connections grow because pads cut off from their pour now get a connection each. Open connections fall by 43 % over the
three seeds (1405 → 802); `failed` rises to meet them, since it was the undercount (the two differ by a few where one
ratsnest line stands for a connection with two failed ends). *Not better:* glasgow-revC3, 95 → 101 open over the three seeds:
its pours are on the inner layers and hardly cut (4 pads at the mid-negotiation check), so it pays for the pour checks and
gains nothing. Boards that end on the budget spend part of it on the rounds; where the first negotiation is still going at
55 % of the budget with many pads cut off, it ends earlier than before (mobo, corne-cherry, revD1, cynthion, tinytapeout).

Completion by this measure (open connections against the connections of the run, seed 1): nrfmicro 97.6 %, the three small
boards 100 %, buspirate5-rev10 99.3 % (100 % in seed 2), lumenpnp-mobo 98.5 %, sweep-v2.2 93.9 %, corne-cherry 99.1 %,
glasgow-revC3 95.3 %, glasgow-revD1 91.9 %, cynthion 95.6 %, tinytapeout-demo 92.6 %. No board that was under 100 % reaches it
in all three seeds except lumenpnp-ringlight.

What is left, by cause (seed 1; `CADLAB_BENCH_VERBOSE=1` lists every connection):

- *Open in the original too* (sweep-v2.2, 3): `gnd`, `raw` and `vcc` join the two halves of the split keyboard, 19, 190 and
  196 mm apart across the mouse-bite row. `CADLAB_BENCH_ORIGINAL=1` shows the same three open on the board as designed, and
  no other board has any: a residual of the design, not of the router.
- *Infeasible under the board's rules* (cynthion, 8): the sense pads inside the force pads of the Kelvin shunts (D72: KiCad's
  DRC reports the original's tracks there as shorts).
- *The USB-C D+/D− crossing* (nrfmicro, 2–3): `DATA-` J1.A7 to J1.B7 and on to U1.29, in 0.2 mm tracks at 0.2 mm clearance;
  the report names pad D1.1 and the routing of CC1, VBUS and GND as what is in the way. The original board routes the net
  (two vias behind the receptacle, 0.2 mm tracks under the same rules), so it is feasible and the router's miss. Unchanged
  by this pass and not diagnosed further in it (D72 looked at the crossover via's depth).
- *Pads the routing still cuts off from a pour* (6 over the boards: buspirate5 1, mobo 1, cynthion 3, tinytapeout 1): reported
  as such; the rounds ran out (budget, or a round that made things worse and was undone).
- *Fine-pitch pads with no exit left* (tinytapeout-demo's QFN-80 7, mobo's LQFP 1, corne-cherry's receptacle CC1 1): boxed in
  by neighbors' escapes and routing, as before D80.
- *Congestion and budget*: the rest (glasgow-revC3 41, glasgow-revD1 139, cynthion about 20, tinytapeout-demo about 20, mobo 8,
  corne-cherry 3, buspirate5 3). Both glasgow boards and cynthion, tinytapeout-demo, mobo and corne-cherry end on the 200 M
  budget with negotiation unfinished; whether more budget closes them was not measured in this pass.

**Corpus re-route, D84** (grid lines along the finest rows, exits that turn by less than onto a line, finer grids
after a run that leaves a few connections; same command, budget, seeds, threads
and machine as D80, whose *after* is the *before* here: reproduced first, to the connection. Both repeat exactly.
On 1 thread the result is the same, work included, for nrfmicro (seeds 1 to 3), lumenpnp-ringlight (1),
buspirate5-rev10 (1, 2), corne-cherry (1, 3), tinytapeout-demo (1, 2), cynthion (2), lumenpnp-mobo (1) and
glasgow-revD1 (1); on glasgow-revC3 (1) tracks, vias and length are the same and `stats.work` differs by 1108 units
of 201 M between 1 or 2 threads and 4 or 8, which the version before does not show there (not diagnosed, D84).
DRC errors other than unrouted: 0 on every board and seed, before and after.)

| board | layers | nets (two pads or more) | connections (seed 1) | open before | open after | failed before | failed after | vias (seed 1) | length (seed 1) | work (seed 1) |
|---|---|---|---|---|---|---|---|---|---|---|
| nrfmicro | 2 | 41 | 85 | 2/2/3 | **0/0/0** | 2/2/3 | 0/0/0 | 37 → 46 | 447.8 → 453.6 mm | 7.9 → 99.0 M |
| buspirate-flash-sop | 2 | 13 | 47 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 4 | 460.2 mm | 1.6 M |
| buspirate-rs232 | 2 | 33 | 43 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 10 | 280.9 mm | 1.9 M |
| buspirate5-rev10 | 4 | 183 | 540 → 538 | 4/0/7 | 2/4/1 | 4/0/7 | 2/3/5 | 304 → 313 | 3044 → 3053 mm | 173.1 → 176.6 M |
| lumenpnp-ringlight | 2 | 12 | 39 | 0/0/0 | 0/0/0 | 0/0/0 | 0/0/0 | 2 | 180.4 mm | 1.4 M |
| lumenpnp-mobo | 4 | 206 | 521 | 8/14/14 | 8/14/14 | 10/16/17 | 10/16/17 | 341 | 5518 mm | 204 M (budget) |
| sweep-v2.2 | 2 | 43 | 49 | 3/3/3 | 3/3/3 | 3/3/3 | 3/3/3 | 21 | 2161 mm | 128.9 M |
| corne-cherry | 2 | 152 | 445 → 443 | 4/5/5 | **2/5/2** | 4/5/5 | 2/5/2 | 488 → 464 | 8393 → 8117 mm | 211 → 213 M (budget) |
| glasgow-revC3 | 4 | 226 | 739 → 744 | 35/32/34 | 35/35/34 | 41/37/41 | 36/39/35 | 422 → 413 | 3398 → 3424 mm | 210 → 201 M (budget) |
| glasgow-revD1 | 6 | 561 | 1701 → 1703 | 137/145/170 | **122/131/124** | 139/147/173 | 131/135/128 | 1035 → 1045 | 8663 → 8813 mm | 221 → 221 M (budget) |
| cynthion | 4 | 337 | 722 | 32/27/33 | 30/34/31 | 33/27/35 | 30/34/31 | 467 → 481 | 3166 → 3201 mm | 202 → 213 M (budget) |
| tinytapeout-demo | 4 | 137 | 393 → 403 | 29/23/28 | **25/10/13** | 29/23/29 | 26/14/18 | 265 → 241 | 4384 → 4884 mm | 200 → 204 M (budget) |
| all twelve | | | | 254/251/297 | **227/236/222** | 265/260/313 | 240/249/239 | | | |

Open connections over the three seeds: 802 → 685 (−15 %). By mechanism:

- *nrfmicro, 7 → 0: the finer grids.* The diagnosis first. Routed alone, D+ and D− join their pads (6 of 6);
  with CC1 and CC2 one D+ connection fails, with VBUS and the diode's net two: the crossover is not what fails,
  the room behind the receptacle is. D− should loop around the D+ crossover via, and the diode pad D1.1, 1.9 mm
  behind the receptacle's pads, leaves that loop 0.18 mm where 0.2 mm is asked; the way around the diode pad
  encloses it. What the designer routed there is fitted to within 10 µm (in the original, a D− track passes
  0.608 mm from the center of a D+ via and the diode's track 0.605 mm from a D− via, 0.6 mm asked; the lanes on
  both sides of the diode pad are 0.15 mm wide for a track's center, one of them between two lines of the 0.2 mm
  grid), with D+ joined right under the pads on the back and D− taken past the diode to two vias 2.75 mm out. The
  0.2 mm grid cannot hold that, negotiation never settles (20 to 40 points still shared when it stops), the
  gridless retry finds a path through CC1 and D+ and fails one more connection routing them again. On a grid of
  0.133, 0.1 or 0.067 mm it settles: seeds 1 to 12 all route to 100 % (2 to 12 s, 15 to 99 M). *Tried and
  dropped:* a crossover that gives both D+ pads a via and takes both D− exits past them and the diode pad when
  the loop has no room. Alone it moved the failures to the diode's net, VBUS and VBAT (4/3/2 failed over seeds 1
  to 3); with the finer grids it routed 11 of 12 seeds, where the finer grids alone route 12. It is not in the code.
- *tinytapeout-demo, 80 → 48.* The QFN-80's rows are on grid lines now (the board gets 110 fanout items instead of
  150; before, 10 of the QFN's 75 pads had no exit and 5 to 7 connections failed on them in every seed, now none).
  One pad of the USB receptacle (J1.B7) that the escape planning served before is missed at the new origin; it
  routes in all three seeds. What is left in seed 1 is pours: the 25 open connections are all on GND and +3V3.
- *corne-cherry, 14 → 9.* CC1 of the right receptacle gets out in every seed (an exit that turns by 0.25 mm, onto
  no grid line, and takes a stub to a cell).
- *glasgow-revD1, 452 → 377; glasgow-revC3, 101 → 104; cynthion, 92 → 95; buspirate5-rev10, 11 → 7.* Their grid
  origins moved by 0.03 to 0.1 mm along x (rows whose pitch is a multiple of the grid's), which is another
  negotiation from the first iteration on. Nothing else in this pass touches them, so these four are the spread of
  such a change, not a gain or a loss to count on: revC3 and cynthion end 3 connections worse, each inside what
  their three seeds span (32 to 35 and 27 to 34). One thing on revD1 is a real loss: the net-tie pad NT1.2 had grid
  access at the old origin and has none at the new one (2 connections in every seed, "no legal way out").
- lumenpnp-mobo, sweep-v2.2 and the three small boards route exactly as before.

What is left, by cause:

- *Open in the original too* (sweep-v2.2, 3) and *infeasible under the board's rules* (cynthion, 8): as under D80.
- *Pours* (seed 1/2/3: tinytapeout-demo 25/0/0, lumenpnp-mobo 1/2/2, buspirate5-rev10 0/2/0, cynthion 1/0/0,
  corne-cherry 0/0/1 reported as cut off; more of the "no path" lines are tracks ending on a cut-off piece).
  Looked at with the phase timeline (`CADLAB_BENCH_TRACE`), not fixed. A pour round plans the cut-off net again
  whole, so its new routing cuts other places: on tinytapeout-demo, seed 1, three rounds each planned two nets
  again (what is open is GND and +3V3) and a fourth, after the retries, was undone for leaving more open, at 25
  open connections with none failed in the plan. On lumenpnp-mobo the one round ends where negotiation's share of the budget does
  (149 M of 200 M) with 9 open, and nothing later may plan again. On buspirate5-rev10, seed 2, the third check
  finds more open than the second and ends the rounds, at 10 open before optimization (4 at the end of the run).
  Ruled out as the cause on tinytapeout-demo, seed 1: optimization after the last check (25 open before it, 25 at
  the end). The likely fix is adding connections for the cut-off pieces and keeping the net's routing; it was not
  built, and D80 dropped the nearest thing it tried (a follow-up run of the whole router on the routed board).
- *A fine-pitch pad with no exit* (lumenpnp-mobo's U5.10, seed 1; D80 listed 9 such pads, the other 8 are gone):
  a GND pad of the LQFP that the F.Cu pour joined when the fanout was planned, so no exit was reserved for it. It
  lies 0.06 mm off its grid line (a track along the line would pass 0.165 mm from U5.11, 0.2 mm asked), so once
  its neighbors' tracks cut the pour it has no grid access. Why the exact gridless search does not reach it either
  was not established; the retry that moves other nets starts at 186 M of the 200 M budget.
- *Congestion and budget*: the rest (glasgow-revD1 about 125, glasgow-revC3 35, cynthion about 22, tinytapeout-demo
  10 to 13 in seeds 2 and 3, lumenpnp-mobo 6 to 12, corne-cherry 2 to 5, buspirate5-rev10 1 to 2). All seven end on
  the 200 M budget or within 12 % of it.

**Native router and guide, D85** (`router: guide`, "guide as a backend" above; the shared 64-core Linux machine).
Both routers behind the same commands, on the seven generated boards and the twelve corpus boards, measured the
same way: open connections on the routed board with its pours filled again by cadlab (`drc.unrouted`, D80), DRC
errors other than unrouted by code, vias, track length, wall time.

- *Native*: seeds 1, 2 and 3 (`CADLAB_BENCH_SEED`), 4 threads; the generated boards with the 120 s budget (never
  reached), the corpus with `CADLAB_BENCH_WORK=200000000` as under D84, whose numbers these runs give again to
  the connection (227 / 236 / 222 open). On one thread it took 35, 36 and 50 s on buspirate5-rev10,
  lumenpnp-mobo and glasgow-revC3 (same results), so its threads are not what separates the two.
- *guide* has no seed, so one run stands for the three: it is the same on every run, with 1 and 4 threads in the
  pool around it and from one build to the next (checked on every board that was run twice). It has no budget
  either, so each board is a process of its own under `timeout 1200`: 7 to 20 times what the native run took on
  the large corpus boards (56 to 180 s), 100 times and more on the others. A run killed there has no result at
  all, since guide returns nothing before it ends. It uses one thread; three or four such runs shared the machine
  at a time, and times moved by up to a factor of two with what else ran (fine-4l 35 and 68 s, stm32-2l 110 and
  192 s for the same result), so they say how many times slower, not how many percent.
- Two rows for guide. *guide* is what `router: guide` runs: no via in a pad, the native router's rule, kept by
  the adapter. *guide, vias in pads* is guide as it routes on its own (`CADLAB_BENCH_VIA_IN_PAD=1`, which no
  command offers); *in SMD pads* counts the vias centred in the box of a surface-mount pad of their net.
- Commands: `CADLAB_BENCH_SEED=<n> route_bench <board>` and `CADLAB_BENCH_SEED=<n> CADLAB_BENCH_WORK=200000000
  route_bench --corpus <board>` for the native router; `CADLAB_BENCH_ROUTER=guide [CADLAB_BENCH_VIA_IN_PAD=1]
  timeout 1200 route_bench [--corpus] <board>` for guide.

| board | router | connections | open | completion | vias | in SMD pads | length | time | DRC errors |
|---|---|---|---|---|---|---|---|---|---|
| attiny-2l | native, seeds 1 / 2 / 3 | 22 | 0 | 100.0% | 4 | 0 | 104.1 / 104.0 / 104.0 mm | 0.1 s | 0 |
| | guide | 22 | 0 | 100.0% | 0 | 0 | 127.8 mm | 0.0 s | 0 |
| | guide, vias in pads | 22 | 0 | 100.0% | 0 | 0 | 127.8 mm | 0.0 s | 0 |
| soic24-2l | native, seeds 1 / 2 / 3 | 184 | 0 | 100.0% | 141 / 139 / 139 | 0 | 1690.6 / 1696.5 / 1698.4 mm | 0.9 / 0.8 / 0.8 s | 0 |
| | guide | 184 | 0 | 100.0% | 139 | 0 | 2146.2 mm | 1.8 s | 0 |
| | guide, vias in pads | 184 | 0 | 100.0% | 148 | 40 | 2042.2 mm | 1.7 s | 0 |
| stm32-2l | native, seeds 1 / 2 / 3 | 109 | 0 | 100.0% | 58 / 57 / 64 | 0 | 970.4 / 937.5 / 945.3 mm | 1.1 / 1.3 / 1.3 s | 0 |
| | guide | 109 | 0 | 100.0% | 95 | 0 | 1139.5 mm | 81.8 s | 0 |
| | guide, vias in pads | 109 | 0 | 100.0% | 113 | 11 | 1157.0 mm | 109.5 s | 0 |
| stm32-4l | native, seeds 1 / 2 / 3 | 109 | 0 | 100.0% | 54 / 57 / 53 | 0 | 902.1 / 869.4 / 892.6 mm | 2.5 / 2.2 / 1.7 s | 0 |
| | guide | 109 | 0 | 100.0% | 48 | 0 | 962.9 mm | 11.0 s | 0 |
| | guide, vias in pads | 109 | 0 | 100.0% | 50 | 9 | 984.1 mm | 11.2 s | 0 |
| qfp-qfn-4l | native, seeds 1 / 2 / 3 | 155 | 0 | 100.0% | 128 / 131 / 126 | 0 | 1934.5 / 1923.3 / 1922.6 mm | 7.4 / 5.5 / 7.5 s | 0 |
| | guide | 155 | 0 | 100.0% | 181 | 0 | 1830.4 mm | 47.5 s | 0 |
| | guide, vias in pads | 155 | 0 | 100.0% | 178 | 43 | 1846.1 mm | 29.3 s | 0 |
| fine-4l | native, seeds 1 / 2 / 3 | 100 | 0 | 100.0% | 103 / 110 / 106 | 0 | 818.8 / 815.0 / 833.9 mm | 1.8 / 2.0 / 2.1 s | 0 |
| | guide | 100 | 0 | 100.0% | 100 | 0 | 971.8 mm | 137.5 s | 0 |
| | guide, vias in pads | 100 | 0 | 100.0% | 106 | 52 | 904.1 mm | 35.2 s | 0 |
| bga144-4l | native, seeds 1 / 2 / 3 | 141 | 0 | 100.0% | 154 / 147 / 153 | 0 | 2211.2 / 2209.9 / 2207.1 mm | 11.5 / 10.6 / 8.8 s | 0 |
| | guide | 141 | 14 | 90.1% | 177 | 0 | 2336.6 mm | 127.2 s | 0 |
| | guide, vias in pads | 141 | 14 | 90.1% | 168 | 21 | 2365.7 mm | 80.7 s | 0 |

| board | layers | router | open connections | vias | in SMD pads | length | time | DRC errors |
|---|---|---|---|---|---|---|---|---|
| nrfmicro | 2 | native, seeds 1 / 2 / 3 | 0 | 46 / 46 / 49 | 0 | 453.6 / 453.6 / 441.8 mm | 24.1 / 2.3 / 13.8 s | 0 |
| | | guide | 15 | 49 | 0 | 461.3 mm | 69.5 s | 0 |
| | | guide, vias in pads | 14 | 34 | 8 | 406.5 mm | 26.4 s | 0 |
| buspirate-flash-sop | 2 | native, seeds 1 / 2 / 3 | 0 | 4 | 0 | 460.2 / 460.2 / 461.7 mm | 1.5 / 0.5 / 1.0 s | 0 |
| | | guide | 0 | 7 | 0 | 522.7 mm | 2.8 s | 0 |
| | | guide, vias in pads | 0 | 7 | 0 | 522.0 mm | 2.9 s | 0 |
| buspirate-rs232 | 2 | native, seeds 1 / 2 / 3 | 0 | 10 | 0 | 280.9 mm | 1.2 / 0.5 / 0.9 s | 0 |
| | | guide | 0 | 6 | 0 | 358.3 mm | 0.5 s | 0 |
| | | guide, vias in pads | 0 | 8 | 0 | 372.6 mm | 5.4 s | 0 |
| buspirate5-rev10 | 4 | native, seeds 1 / 2 / 3 | 2 / 4 / 1 | 313 / 302 / 324 | 0 | 3053.2 / 3172.6 / 3074.2 mm | 66.1 / 57.3 / 56.4 s | 0 |
| | | guide | no result at 1200 s; 26 in a longer run | 390 | 2 | 3209.2 mm | 2351.4 s | 0 |
| | | guide, vias in pads | no result at 1200 s; 17 in a longer run | 444 | 175 | 3172.4 mm | 1415.8 s | 0 |
| lumenpnp-ringlight | 2 | native, seeds 1 / 2 / 3 | 0 | 2 / 4 / 4 | 0 | 180.4 / 181.1 / 181.1 mm | 1.1 / 1.5 / 1.2 s | 0 |
| | | guide | 0 | 1 | 0 | 208.4 mm | 0.7 s | 0 |
| | | guide, vias in pads | 0 | 8 | 8 | 191.7 mm | 0.6 s | 0 |
| lumenpnp-mobo | 4 | native, seeds 1 / 2 / 3 | 8 / 14 / 14 | 341 / 348 / 324 | 0 | 5517.6 / 5420.2 / 5321.5 mm | 45.3 / 73.5 / 69.8 s | 0 |
| | | guide | 21 | 474 | 3 | 6485.0 mm | 558.7 s | 0 |
| | | guide, vias in pads | 21 | 436 | 129 | 6249.1 mm | 625.0 s | 0 |
| sweep-v2.2 | 2 | native, seeds 1 / 2 / 3 | 3 | 21 / 26 / 28 | 0 | 2160.9 / 2159.3 / 2159.0 mm | 56.6 / 87.5 / 96.3 s | 0 |
| | | guide | 8 | 42 | 0 | 2194.8 mm | 67.4 s | 0 |
| | | guide, vias in pads | 8 | 42 | 0 | 2194.8 mm | 63.9 s | 0 |
| corne-cherry | 2 | native, seeds 1 / 2 / 3 | 2 / 5 / 2 | 464 / 543 / 449 | 0 | 8116.8 / 8112.0 / 8070.7 mm | 56.2 / 50.9 / 50.3 s | 0 |
| | | guide | 95 | 243 | 5 | 3816.8 mm | 270.3 s | 0 |
| | | guide, vias in pads | 87 | 297 | 164 | 3931.6 mm | 198.9 s | 0 |
| glasgow-revC3 | 4 | native, seeds 1 / 2 / 3 | 35 / 35 / 34 | 413 / 433 / 414 | 0 | 3423.6 / 3480.8 / 3420.9 mm | 57.9 / 85.1 / 94.1 s | 0 |
| | | guide | not run (the run below was killed) | | | | | |
| | | guide, vias in pads | no result: killed at 1200 s | | | | > 1200 s | |
| glasgow-revD1 | 6 | native, seeds 1 / 2 / 3 | 122 / 131 / 124 | 1045 / 1041 / 1022 | 0 | 8812.7 / 8800.8 / 8744.4 mm | 96.3 / 118.8 / 180.0 s | 0 |
| | | guide | not run (the run below was killed) | | | | | |
| | | guide, vias in pads | no result: killed at 1200 s | | | | > 1200 s | |
| cynthion | 4 | native, seeds 1 / 2 / 3 | 30 / 34 / 31 | 481 / 481 / 464 | 0 | 3201.3 / 3164.5 / 3085.9 mm | 91.4 / 100.2 / 78.4 s | 0 |
| | | guide | no result at 1200 s; 28 in a longer run | 526 | 0 | 3544.9 mm | 1560.2 s | 0 |
| | | guide, vias in pads | 8 | 613 | 353 | 3308.0 mm | 348.6 s | 0 |
| tinytapeout-demo | 4 | native, seeds 1 / 2 / 3 | 25 / 10 / 13 | 241 / 278 / 279 | 0 | 4884.1 / 4854.0 / 4821.6 mm | 84.8 / 70.0 / 59.3 s | 0 |
| | | guide | no result: killed at 1200 s | | | | > 1200 s | |
| | | guide, vias in pads | no result: killed at 1200 s | | | | > 1200 s | |

The native rows of buspirate5-rev10, lumenpnp-mobo and glasgow-revC3 were measured again after the outline fix
below and are identical. Under the rule the few vias "in SMD pads" (corne-cherry 5, lumenpnp-mobo 3,
buspirate5-rev10 2) are the box test's: corne-cherry's lie in the boxes of rotated pads and none overlaps their
copper (counted by overlap in a run of its own); the other two were not recounted. Where guide had no result at
1200 s on buspirate5-rev10 and cynthion, a run of its own with a limit of 1800 or 3600 s shows what it would
have returned; the two glasgow boards and tinytapeout-demo did not get one.

What the numbers say:

- *Generated boards.* The native router routes all seven to 100 % on every seed. guide routes six, and leaves
  14 of bga144-4l's 141 connections open: one net, VCC with 15 pads, which it reports unrouted as a whole. It
  takes 2 to 70 times as long (soic24-2l 1.8 s against 0.8, stm32-2l 82 s against 1.1 to 1.3, fine-4l 138 s
  against 1.8 to 2.1), 7 to 27 % more track on six boards (5 % less on qfp-qfn-4l) and from 10 % fewer vias
  (stm32-4l) to 60 % more (stm32-2l, 95 against 57 to 64). attiny-2l is the exception in time (35 ms against 60
  to 110 ms) and has no via at all.
- *Corpus under cadlab's rules* (the rows "guide"). At the 1200 s limit guide has a result on seven of the
  twelve boards. The three small ones the native router completes are complete (buspirate-flash-sop,
  buspirate-rs232, lumenpnp-ringlight). On the other four the native router is ahead: nrfmicro 15 open against
  0, sweep-v2.2 8 against 3 (the 3 that the original board leaves open too), corne-cherry 95 against 2 to 5,
  lumenpnp-mobo 21 against 8 to 14. On those seven boards together guide leaves 139 connections open
  and the native router 13 to 22. The longer runs: buspirate5-rev10 26 open after 2351 s (native 1 to 4 in 56 to
  66 s), cynthion 28 after 1560 s (native 30 to 34 in 78 to 100 s): the one board where guide under the same
  rules is level with the native router, at 16 to 20 times the time. tinytapeout-demo has no result at 1200 s,
  and glasgow-revC3 none either way (glasgow-revD1 was killed with vias in pads allowed and not run again).
- *corne-cherry* is routed with zones as wide tracks (933 zones on some 150 nets). guide takes every such net for a
  plane net and reports 80 of them "with zones in pieces"; it has an option to treat zones as fills only
  (`planes`), which the adapter leaves on, and a run with it off was stopped after 10 minutes without a result.
- *guide as it is* (the rows "vias in pads"): cynthion 8 open in 349 s, where the native router leaves 30 to 34
  and guide under the rule 28: the 8 are the Kelvin sense pads that no routing can reach under the board's rules
  (D72). This is the one result where guide is clearly better, and it is bought with vias in pads: 353 of its
  613 vias are in SMD pads, the ECP5's balls among them. Elsewhere vias in pads change little (lumenpnp-mobo 21
  open, buspirate5-rev10 17 after 1416 s, corne-cherry 87, nrfmicro 14), and a tenth to more than half of
  guide's vias are in pads (stm32-2l 11 of 113, soic24-2l 40 of 148, fine-4l 52 of 106).
- *DRC.* Zero errors other than unrouted on every result of either router, and the verification took nothing
  out of guide's results (`stats.drc_removed` 0 on every run): within the rules that could be given to it,
  what guide routes is legal by cadlab's DRC. The one exception was cadlab's fault and is fixed here:
  `board::exact::item_core` gave an exposed pad the outline of the thermal via pad at its center (two pads of
  one footprint with the same number at the same position), so guide was told that the 3.2 mm pad under
  buspirate5-rev10's RP2040 was a 0.6 mm circle, routed two nets across it, and the verification took 68 items
  out (`drc.short`). lumenpnp-mobo (J7.2, U16.9) and glasgow-revC3 (U1.57) have such pads too. The DRC measures
  exact distances with the same function; its error counts on the twelve boards as imported are the same
  before and after.
- *Pours.* guide's fill and cadlab's disagree on a pad on five boards (buspirate-flash-sop C1.2,
  buspirate-rs232 J2.5, cynthion 2 nets, lumenpnp-mobo 1 to 2, corne-cherry 1): guide reports the net routed
  and the refilled board has it open. A pour round joins it on the two small boards, cynthion and
  corne-cherry, at the price of one more run of guide; on lumenpnp-mobo that run is 250 s of 560 for two nets
  of 51 pads together, which it routes in part.
- *Determinism.* Every board run more than once gave the same tracks, vias, length and expanded nodes: with 1
  and 4 threads in the pool, under different loads and from three builds of the adapter (the generated boards,
  nrfmicro, the two small buspirate boards, sweep-v2.2, cynthion). guide starts no thread of its own.

The default is unchanged, and what the numbers say about when it could change is in DECISIONS D85.

With arcs (`route.all {arcs: true, arc_radius: 0.8mm}`) the ATtiny board routes the same with its bends rounded,
DRC-clean, exported as arcs to Gerber and KiCad (`tests/route.rs`, `arc_corners_are_drc_clean_and_exported`).

The older `bench_generated_boards` in `tests/route.rs` (M5 table above) still runs the SOIC grids and the dense
LQFP-32 board.

**Regression check in CI** (D43): `route_benchmark_regression` in `tests/route_bench.rs` routes every board above
except the budget-bound `big-4l` (600 s budget, never reached, so the results are deterministic: identical on
macOS and Linux, and with any number of threads) and compares them with `tests/golden/bench/route.json`. It fails
on any DRC error, an exhausted budget, a lower completion (more unrouted connections or a lower routed share), more
than max(2, 5 %) extra vias or more than 2 % extra track length, and on a large slowdown (the timing rule of
`tests/common/bench.rs`, below and in docs/TESTING.md). A change within tolerance is reported as such, to be
recorded with `CADLAB_BLESS=1` (in release with `RAYON_NUM_THREADS=1`, like the CI job). The `bench` CI job runs
it and uploads `route.json`/`route.md` reports; the default `cargo test` checks that the baseline lists exactly
these boards and that `attiny-2l` routes exactly as recorded.
