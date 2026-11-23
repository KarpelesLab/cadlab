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

**`export.dsn {path?, protect_existing?, resolution?, planes?}`** (`specctra::export`): `(resolution um 10)` by default
(the router's grid; coordinates themselves are written in µm with up to three decimals, i.e. exact nm). Written:

| DSN | from |
|---|---|
| `structure/layer` | copper layers, all `signal`, top to bottom |
| `boundary (path pcb 0 …)` | outer contour, arcs as chords within 1 µm |
| `keepout` on layer `signal` | each cutout (`cutout1`, ...) |
| `structure/plane` | with `planes`: each pour that has a net, as `(plane <net> (polygon <layer> 0 …))` of its outline on each of its copper layers (D102); without it pours are left out |
| `keepout` / `wire_keepout` / `via_keepout` | keep-outs forbidding tracks and vias / tracks / vias, one per layer (`signal` when all); pour- or footprint-only keep-outs are left out with a warning; net exceptions are not written (the external router keeps those nets out too, `export.keepout_net_exception`, D76) |
| `structure/via`, `structure/rule` | via padstacks in use (rules default first); rules track width and clearance |
| `placement` | each footprint: image = footprint name, position, `front`/`back`, rotation, `lock_type position` when locked, `PN` = part; mounting holes as components `H1`… (locked) |
| `library/image` | pins in footprint-local coordinates (pin ID = pad number; repeated or empty numbers become `<number>@<index>`), courtyard as outline, non-plated holes as image keep-outs; a non-plated mounting hole is an image with only a keep-out |
| `library/padstack` | one per distinct pad: `smd_…` on `F.Cu`, `tht_d<drill>_…` on every layer; rectangles as `rect` (a quarter turn swaps the sides), circles as `circle`, ovals as `path` with the minor width, round rectangles as polygons circumscribing the corner arcs (4 segments per corner), other pad angles as rotated polygons, polygon pads (imported KiCad custom pads), trapezoids and chamfered rectangles as `poly_<hash>` polygons (rounded corners within 1 µm outside); vias `via_<diameter>_<drill>[_<from>-<to>]` in µm |
| `network/net`, `network/class` | nets with pins `U1-3`; one class per net class (track width, clearance, `use_via`) plus `default` from the rules |
| `wiring` | tracks as `wire (path …)` (arcs as chords), vias; `(type protect)` when locked, or all with `protect_existing` |

Back-side parts: Specctra mirrors the image across its Y axis and then rotates counter-clockwise by the
placement angle, which is cadlab's own transform, so rotations are written unchanged
(`specctra::dsn::place_point`). Zones are not written by default: the router wires their nets pad to pad, and
the pours are filled again after the import. With `planes` each pour with a net is written as a plane, by its
outline: the router then takes a pad of that net as connected where it reaches the plane (freerouting 2.5 does,
and lets other nets cross it), which is what cadlab's own routers do with a pour. The outline is not the fill:
priorities, clearances, thermal reliefs and the islands a fill leaves are not written, so a pad the router
counts as connected can be one the filled pour does not reach (`drc.unrouted` after the import says so).
Netless or unplaced items stay out.

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
its session (a net or a few connections missing from `network_out`); such runs are retried, up to 5 times.
freerouting 2.5.0 (2026-11-17, Java 25) routes both completely at the first attempt, with two tracks of the STM32
board at 0.125 mm, under the board's minimum of 0.15 mm, which a DSN does not carry: the test reports and allows
`drc.track_width` and nothing else. Also
tested without the oracle: a golden DSN, the reader round trip (equal data, every pin at its pad center through
`place_point`, nets complete), and a hand-written session (`tests/fixtures/ldo.ses`).

Limits: no DSN-to-project import (the reader is a library function used by tests); arcs (`qarc`) and polygon
wires in sessions are skipped; Specctra strings cannot contain `"` (written as `'`, with a warning). Not
expressed in the DSN, and checked by cadlab's DRC after the import: zone fills (outlines only, with `planes`),
the minimum track width, the copper-to-edge, hole-to-hole and copper-to-hole clearances, custom rules (a
clearance or a track width for an area, a net, a footprint or a pair of them, items forbidden in an area), the
clearances of single pads and footprints, a keep-out's exception for some nets, differential pairs and length
rules. What that costs an external router on real boards is measured under "freerouting on the corpus" below.

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

## guide as a backend (`router: guide`, D85, D88, D91, D93, D95, D96, D97, D101, D104, D106, D108, D110, D113, D114, D115, D116, D117, D118, D121, D122, D123, D124, D125, D126, D127, D128)

[guide](https://crates.io/crates/guide) (MIT, the same owner as cadlab, a standalone placement and routing engine
in its own repository, as polyclip is for polygons) has a PCB router, `guide::pcb::route`. `router: guide` on
`route.all` and `route.nets` runs it in place of cadlab's own search, through `router::guide`, an adapter in the
library (no command, no file in between: the board goes to guide's `pcb::Design` and its tracks and vias come back
through guide's API). The default router is unchanged, and so is everything the native router does. The dependency
is `guide = { version = "0.0.11", default-features = false, features = ["pcb"] }`: its only dependency is polyclip
0.0.6, the version cadlab uses, so the tree has one polyclip and nothing new besides guide itself. 0.0.3 was the
release with run control and the host's board model (D88), 0.0.4 the one for a host with a budget that fills its
zones itself (D91), 0.0.5 the one whose every phase keeps the deadline and that shares a budget out between
routing and what comes after (D93), 0.0.6 the one that plans the ways out of fine rows and deep balls in a run
with a budget, moves a net aside before it takes one out, and names the nets its own search found no way for
(D95), 0.0.7 the one whose plan of a run takes a third of the time (D96), 0.0.8 the one in which a net that
found no way in its corridor keeps what it reaches with nothing moved before any net crosses another's routes,
and that joins the pads its routes cut off the fills it planned on, 0.0.9 that one without its via on a via
(D97), 0.0.10 the one whose passes of rip-up end where they have stopped gaining and that settles what they leave
open by chains of moves kept only if the board has less open for them, 0.0.11 that one without its chains for
the pads cut off their pours, which cost a board with every net routed a third of its time (D101), 0.0.12 the one
that negotiates for each connection its passes leave open before its chains, every net near it giving way a
little (D104), 0.0.13 the one whose routes that keep what they reach begin another tree from a terminal left out,
for a net's pads that nothing joins to its first, 0.0.14 the one that goes on from an earlier run's routes and may
move them (D106); what this
section says of 0.0.2 is in D85.

**What goes to guide** (`router::guide::adapt`):

| cadlab | guide |
|---|---|
| copper layers of the stackup | `RuleSpec::layers`, by position |
| nets (circuit, board copper, zones) | `NetId` by position in name order; `NetSpec { route, track_width, via }` from the net's class (the pair width for a pair's nets); the nets with open connections in scope have `route`, and in the first run the nets with zones in scope too, open or not (`first_nets`, D97: a net that cadlab's fill joins whole has no open connection, and the routes of the others cut its pads off their pour) |
| rules | `RuleSpec`: `min_track_width`, `clearance`, `min_clearance`, `hole_clearance`, `hole_to_hole`, `min_drill`, `min_annular_ring`; `copper_to_edge` as `Design::edge_clearance` |
| net classes with a clearance | `NetClass` with that clearance override (guide resolves the larger of two nets', as the DRC does) |
| custom clearance rules that are an area's (D61, D96): scope `in_courtyard` or `touches_area` alone, with or without a layer, of kinds that take in tracks and vias (all kinds, or KiCad's "no zone"), between the scope and anything or the scope and itself | `RuleSpec::areas`: one `RuleArea` per courtyard or area outline, the last rule in board order first (the DRC's last rule wins, guide's first area). guide applies it between two pieces of copper that both touch the area, in place of their own clearances; the DRC applies a rule when either item is in its scope, and the later rule of the two items'. So an area has the largest of its rule's clearance, those of the existing pads, tracks, vias and drawings in it that a later rule holds, and those of later rules that can hold new copper and whose courtyards or areas meet it; a rule that loosens the clearance is routed the stricter way between a track in the area and one outside. The track width of a rule is not passed (guide routes a net at one width) |
| custom clearance rules on existing pads and copper drawings (D96): the last rule holding each, as the DRC resolves it (`board::clearance::rule_matches`), a rule for the pads of a footprint and an area's rule alike | `Pad::clearance` and `Zone::clearance`, the item's own clearance, in place of its local one (a rule comes before a local clearance in the DRC too), raised to a later rule's that can hold new copper within 1 mm of the item. A pad stack has the largest of its shapes' |
| differential pair gap (`diff_pair_gap`) | `RuleSpec::pairs`: the smaller of the gap and the clearance between the two nets, as the DRC allows |
| pads | `drc::Pad` with the exact shape where the pad has one (`board::exact`: a circle, a segment or a polygon grown by a radius, which is guide's shape model too; chamfered round rectangles as their polygon), its layers, its hole (round or slot) and its own clearance: a custom rule's (above), else its local one |
| pad stacks (another shape on some layers) | one `Pad` with the other shapes in `Pad::stack`, each on its layers, and the hole: one piece of copper through its barrel |
| unplated holes, mounting holes | a `Pad` with a hole and no copper layer when the rules set a `hole_clearance`; without one the DRC and guide only forbid copper over the hole while the native router keeps the copper clearance from it, so the hole is given as copper of no net on every layer and guide's routes keep that distance too (0.2 mm instead of 75 µm in `guide_keeps_clear_of_unplated_holes`) |
| no via in a pad (the native router's rule; not a DRC rule) | `RouteOptions::vias_in_pads = false`: vias keep out of pads without a hole and guide's escapes are dog-bones (`RouteOptions::via_in_pad` of cadlab, which no command offers, leaves guide its vias in pads, for measuring). The heat pads (`board::thermal::classes`, `PadClass::Heat`: the exposed pad under a package, a tab) without a hole are named in `Zones::via_pads` (guide 0.0.19, D115): guide lets a via of the pad's own net into one, all inside its box, where no straight track of the net leaves the pad and none joins it to a pad of its net beside it. The rule is for the pads that leads are soldered to, and such a pad's own board joins it to its plane by vias in it |
| existing tracks (arcs with their middle) and vias (their layer span) | `drc::Track`, `drc::Via`: fixed copper, locked or not |
| copper drawings and copper text | fixed `drc::Zone` of their net, or `Design::obstacles` without one |
| net ties | `Board::ties`: the copper drawing between the pads of a tie group is an item its nets may touch, as in the DRC (`board::tied`). The tie's pads are not such items: a track of one net keeps its clearance from the other net's pad, in the DRC too |
| keep-outs (`no_tracks`, `no_vias`, layers) and the nets they except (`allow_nets`, D76) | `drc::Keepout` with `except`. `no_pours` has nothing to say to guide: the fills are cadlab's |
| outline and cutouts | `Design::outline` from `pcb::edges`: one edge per segment of each contour (arcs at the copper tolerance) |
| zones | `Design::zones`: outline, layers, net, priority and the effective fill settings (`zone_params`: clearance, minimum width, pad connection, thermal gap, spoke width and number of spokes), pad overrides through `Zones::set_pad`, and **cadlab's fill** of each zone on the board as it is, island by island, as `Zones::fills`: guide plans a net's stubs on the copper the board has, not on a fill of its own. A zone cadlab fills with nothing is not passed. `CopperZone::plane` is left to guide: the board does not say whether a zone is a plane or a pour between a few pads, and guide takes a zone as a plane unless its net has four pads or fewer and its outline is 32 lattice pitches wide or less (every zone a plane, as before 0.0.4: 86 open against 54 on corne-cherry and 30 against 12 on lumenpnp-mobo at 60 s, D91) |
| the smallest via the rules allow (`min_drill` with `min_annular_ring` around it: the native fanout's small via, `router::smallest_via`) | `RouteOptions::fanout_via`: the via of the dog-bone of a pad trapped in its footprint where the net's own fits nowhere near; every other via is the net's |
| zone filling (cadlab's, D80) | `RouteOptions::fills = false`: guide makes no fill at the end and returns none. `RouteOptions::join = false`: nor does it fill the zones around its routes to join the pieces that fill leaves a net in; cadlab's fill says what is apart, and the second run joins it (D93; measured again with 0.0.6, whose joining no longer stops a run with a work budget: 432 open on the corpus with it against 420, and the small boards take twice as long, D95) |
| what the routes cut off the fills (D97) | `RouteOptions::pour_cuts = true`, guide's default: guide plans on cadlab's fills as they are before routing, keeps count on a grid of what its routes cut off them, and once routing is over joins the pads that are cut off to their fill again by stubs that move nothing. What cadlab's fill of the routed board still leaves apart goes to the second run |
| what the passes of rip-up leave open (D101) | `RouteOptions::settle = true`, guide's default since 0.0.10: a pass ends once every net has had a turn since its routes made the most connections and 2 million nodes have gone since, and each connection still open is then looked for a route across other nets' routes, the nets it crosses cut and joined up again around it at once, three nets deep and twelve in all; kept if the board then has less open, counting the pads the routes cut off their fills, undone to the track if not. The pads cut off are joined by guide's stubs after routing and by the second run, not by chains (0.0.11) |
| what the chains do not settle (D104) | `RouteOptions::negotiate = true`, guide's default since 0.0.12: before its chains each open connection is negotiated for. The net is completed across the routes in its way, a step within a clearance of another net's route costing its length again times a price; the nets it runs into lose what they have within twelve pitches of there and are joined up again within twenty-four, across the others at that price or around them; the price rises by 1.6 a round, and a history by cell makes the places dearer where routes were in each other's way. After twenty-four rounds, thirty-two nets or three million nodes what is still shared is given up, the net the negotiation is for last. Kept if no route is in another's way and the board has less open, as a chain is, and undone to the track if not. The settling comes after the first pass and after each pass on the finer lattice, the chains after the last |
| the ways out of rows of fine pitch planned row by row (D97, D101, D104) | `RouteOptions::row_fanout = false`, guide's default, on every board. They were on for boards of two copper layers with 0.0.10 and 0.0.11 (D101: nrfmicro 11 open with them for 20 at 40 million nodes over three orders, corne-cherry 130 for 140), and make no more with the negotiations of 0.0.12: at 60 s over six orders corne-cherry has 160 open without them for 155 with and nrfmicro 19 for 18, and a board that completes without them has its vias back (the generated stm32-2l: 126 with them, 72 without) |
| `layers` | `RouteOptions::layers`: tracks on these layers only; vias go through the board all the same |
| `seed` | `RouteOptions::seed`, when not 0: another order of the nets, the same result for the same seed. Without one guide routes shortest first |
| differential pairs in scope (`pairs` not false) | `Design::pairs` at the pair's width and gap; a pair's `max_skew` as a `LengthConstraint` |
| length groups whose members are all routed | `Design::lengths`: `target ± tolerance`, or `max_skew = tolerance` without a target |

**What comes back.** guide's tracks (straight, octilinear) and through vias become unlocked tracks and vias of
their nets. Its zone fills are dropped: zone filling stays cadlab's (they are derived data here, and the DRC, the
exports and the count all read cadlab's fill). The result is then treated as the native router's:

- *Verification.* `drc::check_copper` on the board with the new copper; a connected piece of new copper (tracks
  sharing an end, vias at an end) holding a flagged item is taken out, a net's whole new copper after three
  rounds; `stats.drc_removed` counts the items. Nothing the DRC flags is ever applied.
- *A second run.* guide plans on cadlab's fill of the board as it is before routing, and routes change it: a
  track of another net can cut a pad off from its pour (C1.2 of `GND_CHIP` on buspirate-flash-sop, U1.3 of `GND`
  on lumenpnp-ringlight). After the run the zones are filled again by cadlab around the new copper, and guide is
  given the routed board and its fills once more: after a run that ended, for the nets with zones that the board
  still has open, less those whose own search found no way (`Routed::failed`, guide 0.0.6); after a run that its
  deadline stopped, for every net the board has open, those of `Routed::failed` too. A search that failed in a
  run that was cut is not the last word on the routed board: left out, as the nets of a connection whose cause
  is a search that failed were until D95, the twelve corpus boards have 420 connections open at 60 s, and with
  them 401 to 419 (the first run at half the budget for both, D95). The run is kept when it leaves fewer
  connections open (`stats.iterations` is the number of runs of guide, 1 or 2). Not after a run that the work
  budget stopped. Measured without it: one connection open on each of those two boards, which it closes (D88,
  and again with guide 0.0.5 and 0.0.6); and at 60 s with guide 0.0.7, where the first run had 0.45 of the budget
  so that the second had time, 315, 275 and 300 connections open on the twelve corpus boards over three orders
  of the nets against 571, 569 and 480 with one run that has nine tenths of the budget (D96; 353 and 359 against
  598 with guide 0.0.6, D95; 564 to 586 against 681 with 0.0.5, D93; 589 against 714 with 0.0.4, D91). With
  guide 0.0.8 the first run does itself most of what the second did on glasgow-revD1 (a net keeps what it
  reaches with nothing moved before any crosses another), joins the pads it cut off the fills it planned on,
  and has the nets with zones among its nets, open or not, and 0.7 of the budget: the second run is then for
  what cadlab's fill of the routed board still leaves apart, 15 connections of 182 on glasgow-revD1 where it
  made 111 of 325, and one of lumenpnp-mobo's ground where it was asked for 220 stubs (D97; one run alone was
  not measured again).
- *A last run that moves routes* (guide 0.0.14, D106). In the second run the first run's routes are copper that
  does not move: a pad that the fill no longer reaches gets a stub if there is room as the routes lie, and a pad
  that the first run left shut in stays so. In what time (or work) the second run leaves, guide is given every
  net the board still has open and the routes of the runs before as *routes of their nets*
  (`pcb::route_from`, `Adapted::take_back`): a net they join whole has no turn until another moves it, one in
  pieces is completed from them, and any of them may be moved as in a first run. guide says whose routes it took
  back (`Routed::taken_back`: not a track of another width than its net's, an arc, a via of another size or a
  net with nothing to join); its result stands in place of those, the others stay as they were, and a net that
  it did not move comes back as it was given, so that the fill is cut where it was. Kept when the board has
  fewer connections open for it, so no board ends with more than the two runs before it left; at least 3 s must
  be left for it (`stats.iterations` is 1 to 3). On sweep-v2.2, whose second run is cut by the deadline, there is
  no third.
- *More runs that move routes, in other orders* (D108). A run that moves routes ends on its own once its order
  of the nets has no more to give: on nrfmicro after some 9 s, with one connection open and most of the minute left.
  For as long as something is open and 3 s are left, another such run has the same board and every open net
  with the nets in another order (the seed moved by 7919 a run, four more at most), each kept only if the board
  then has fewer connections open (`stats.iterations` is 1 to 7). Under a work budget they go on within it.
- *The stubs that the fill makes needless* (D108). guide gives each pad of a net with a plane inside a stub
  through a via to it, and plans on no pour of an outer layer, which routes cut (planned on them, tinytapeout-demo
  had 153 open for 36 at 40 million nodes). cadlab's fill around the routed board joins many of those pads on an
  outer pour all the same. Once the runs are over and the budget has the time for two fills (2.5 times what the
  first run needed after it), every piece of new copper of a net with zones that is one via and one to four
  tracks and no escape is taken out, the board is filled again, the stubs whose ends are then not on their net's
  largest island are put back, and the result is kept if the board has no more open than before. No rule is
  checked again: copper taken out breaks none. The runs are not shortened for it: a run with no time left keeps
  its stubs (corne-cherry, cynthion, glasgow-revC3 and most orders of tinytapeout-demo at 60 s), which is where
  the vias saved would have cost connections (below, "tried").
- *Crossings in a row of fine pitch* (guide 0.0.15, D108): `RouteOptions::crossings`, guide's default, set by
  name. Where the pads of two nets take turns along a row with no room for one to go round the other's via on
  the row's layer, guide plans the crossing on another layer before it routes, as fixed copper that comes back as
  escapes do.
- *The room of the spokes of pads in pours* (guide 0.0.16, D110; BOARD.md, "What a router must be told"):
  `RouteOptions::spoke_cost` 4, and the spokes as cadlab's fill makes them, `Zones::spokes`. Per pad of a net
  with a zone that holds it, per layer of the zone that the pad is on, where the connection in effect there
  (`zones::connection_on`) is thermal and the pad is soldered (`thermal::classes`): each spoke of
  `zones::Relief::of` (gap, the width fitted to the pad, its directions), from the pad's anchor to the end of
  its rectangle. A track of another net that ends in the room of one costs five times its length: a price and
  no wall. The directions the fill falls back to go over as spare spokes (`Zones::spare_spokes`, guide 0.0.17,
  D113): where guide tells the pads that routes cut off their pours, a pad is joined by the spokes that nothing
  crosses, those among them, and by nothing else. `rules.min_thermal_spokes` goes over as `Zones::min_spokes`: the
  spokes of a pad with no more of them than that cost eight times as much to cross (no pad of the corpus has so few).
- *Every net at once, before guide's passes* (guide 0.0.17, D113): `RouteOptions::whole` in a run with a budget
  of time or of work, in the first run only, on rayon's threads. guide routes every net against every other on
  one grid, as the native router's negotiation does (PathFinder: claimed points at a price that rises, a history
  of the points shared, the wires in another's way routed again until none is), tells the pads that the routes
  cut off their pours and negotiates again for them, routes each net once more for fewer vias, and gives its own
  passes the result as each net's copper, less what its exact check does not allow. It has half of the first
  run's budget, and gives a board up to the passes itself when the first round says a negotiation has no room in
  that (of the corpus, glasgow-revD1). A run with no budget routes as before. Since guide 0.0.19 (D115) the
  grid of that negotiation is a track and its clearance apart on a board of more than two copper layers, twice
  what it was and what a board of two keeps: a quarter of the points, a round at a fifth of the nodes and a via
  at twice the track it cost. glasgow-revD1 is negotiated on it, five rounds in its share, and no board of the
  corpus is given up at 60 s; at 40 million nodes glasgow-revD1 still is.
- *cadlab's own fill while guide routes* (guide 0.0.18, D114): `RunControl::refill` is `host_fills`, the board
  without the copper that the run routes or may move, with the run's tracks and vias on it, filled by
  `board::zones` as any board is, each island of a zone as a `Fill` of guide's. Where guide told the pads that
  its routes cut off their pours by cells of its own, it tells them by those pieces, exactly: asked for where a
  fill is planned on an outer layer, when its negotiation tells the pads cut off and at its rounds of stubs after
  routing, and on any board once more when cleanup is over. With a deadline the fills while routing have 15 %
  of the run's time in all and none is begun that would end after it; the last was made after the deadline too
  if a fill took no more than 3 % of the whole budget (D114), and is made only before it since D115: what it
  tells, the fill that counts the run tells as well (below). With a work budget every fill asked for is made,
  and the result is the same on any machine.
- *The stubs that a pour makes needless* (guide 0.0.18, D114): by that last fill guide takes out the stubs to a
  plane whose pads cadlab's pours join to their net without them, and returns them apart (`Routed::spare`).
  They stay on the board between the runs: a run that has the routes back
  plans a net with a plane on the plane alone, and would make each again on a fuller board. Once the runs are
  over they go with whatever else the fill of the board as it is then makes needless (below). The pass of D108
  for the same, which fills the board twice, stays for what that leaves when there is time.
- *What the fill of the routed board makes needless* (guide 0.0.19, D115): once the runs are over, `needless`
  gives guide's `pcb::needless` the first run's design, every track and via that guide's runs left on the board
  and the fill that counted the last run, island by island (`fills_of`), and has back the vias that the pads of
  their nets are joined without, by the copper itself, and the tracks that those leave leading nowhere. No fill
  is made for it. They are what goes, in place of what each run told by its own fill, all of which is still on
  the board then; the board is filled once without them, and where a connection is open that was not (another
  net's pour may take the room that copper leaves, and the fill that joined a pad is then another), what was
  taken out of that net within 3 mm of the connection goes back in and the board is filled once more; kept if
  it has no more open. With a deadline it is made if there is time for one fill, and the second if there is
  time for it then; the later runs' deadline leaves the time of two where a fill takes no more than 3 % of the
  budget (`SHORT_FILL`): glasgow-revD1's took 5.7 s and the step was not made there, and since D117 it takes
  1.7 s behind the adapter and is. A track from pad to pad stays in this step whatever fill joins the two (the next step is for
  those, below), and a piece of fill with no pad on it and less than a sixteenth of its net's largest is joined to
  nothing for its own sake.
- *The tracks from pad to pad that a pour joins as well* (guide 0.0.20, D116). A net with a pour is joined up
  again by tracks each time the routes of others cut the pour, while those still move, and a pad off its plane is
  taken to the pad beside it where that is nearer than a via; with the routes where they stay, the pour reaches
  many of those pads all the same (lumenpnp-mobo's ground: 402 tracks and 448 mm for 20 vias, where the native
  router, which plans on the pours, has 93 and 124 mm for 21). After the vias are out, a second step asks guide's
  `pcb::needless_between` for every track that the pads of its net are joined without by the fill as it is,
  counts the joints of the pads of the nets with pours (`thermal::joints_of`), takes those tracks out together
  and fills the board. A pad that this leaves short of its spokes (`Joint::starved`) where it was not, or with
  fewer necks than it had and fewer than two, has what was taken from its net within 1.5 mm of it back, as a
  connection that is open now and was not has what was taken from its net within 3 mm, and the board is filled
  once more; kept if it then has no more open and no pad is worse off. Begun only with the time of two fills
  and three counts, for which the later runs' deadline leaves a fill more and one more time what the first run
  needed after it, where a fill takes no more than 3 % of the budget.
- *Shorter ways, where the runs leave the time* (guide 0.0.20, D116). A net keeps the way round another that
  has moved since, and a negotiation leaves every route where the prices of its rounds put it. Once the runs
  are over, guide has the routes back once more, as a run that moves routes has them but with nothing to
  route (a net with a connection open keeps its routes as copper of the board, unless it has a zone), no right
  to take a net out, and `RouteOptions::shorten`: in its cleanup each run of a net's tracks from one pad or
  branch to the next that costs more than the straight way is taken out and looked for again, the others where
  they are, and kept if it is cheaper, a via at three times its routing price. Its result is verified as any
  run's; where a connection is open that was not (a track that went another way may cut a pour, which guide
  tells by an estimate of its fills), the nets with new copper within 1.5 mm of an end of it have the routes
  they had back, as have the nets that are open themselves where they were not and those that the check takes
  copper out of beside them, and the rest is verified again, three times at most;
  kept if the check takes nothing out and the board has no more open. No time is kept for it: it is made when
  the runs end early or the budget leaves room (D116 has what keeping time for it cost). It is a run that moves
  no route, and since guide 0.0.21 (D117) such a run plans its escapes on the board with the routes it is given
  on it. Until then each pad had the escape planned again that a first run had planned for it, and where that
  run had given the place to another net since, the other net's via was cut as the run began: cynthion's run
  for shorter ways came back, every time, with two pads of +2V5 open under its array (which D116 put down to a
  pour that the run had cut) and was not kept. The runs that move routes still plan their escapes that way: the
  net that is cut is routed again round the escape, which is how a ball left shut in comes out (planned round
  the routes there too, glasgow-revC3 had 29 open over four orders at 150 million nodes for 22).
- *Two runs that move routes and gain nothing end them* (`MOVE_STALE`, D117). Another order of the nets closes
  what one left open, and a board whose open connections no order closes had the same asked of it until the
  time was gone: cynthion's eight had three such runs and its run for shorter ways 1.5 s for its searches,
  where after two it has 8.6 s.
- *The tracks end where they are inside the pads of their net* (`pad_ends`, D118). guide routes to a pad's centre;
  once the runs are over, an end of a straight track inside a convex pad of its net is moved back along the track
  for as long as the track's round end stays inside the pad, and a track with both ends so inside one pad goes.
  What is cut is inside the pad's copper, so the board's copper, its fill and its connections are the same; the
  track is then counted as the native router's, which ends at the first point of its grid inside a pad. Not the
  nets of pairs or with a length to keep, nor the pads of a net tie.
- *A track enters a pad no wider than the pad* (`router::guide::neck_at_pads`, guide's `pcb::neck_at_pads`,
  D121), in the result of either router (`router::route`): a new straight track wider than a pad of its net
  (the pad's shorter side as `drc.solid_pad` measures it, `zones::short_side`, to the micrometre under it, and
  never under `min_track_width`) is cut where its middle line comes within half its width and 25 µm of the pad,
  and laid at the pad's width from there to its end in the pad, along the same line. The copper laid is inside
  the copper that was there and the ends are where they were: the same connections, no clearance nearer. The
  25 µm are the 10 µm band in which a pad's joints are read, as much again for what the band and the track are
  drawn outside their true shapes, and 5 µm. Not with `neck: false`, not for arcs, and a pad narrower than the
  minimum track is still met by a track wider than itself.
- *A track moved off a pad's spoke* (`router::guide::spoke_rooms`, D121; BOARD.md, "What a router must be
  told"). Once the runs are over and the needless copper is out, where 1.5 s, two fills and two counts of the
  joints are left: for each pad in a pour that is short of its spokes, one direction of its relief whose room
  holds nothing of another net but tracks and vias of this run; the parts of those tracks that touch the room
  (told by `pcb::neck_at_pads` with the room for a pad) and those vias are taken out, guide has the routes of
  their nets back with the room as a keep-out for every net but the pad's, for 5 s at most, and what it makes
  is verified, filled and counted: kept if the check takes nothing out, no connection is open that was not,
  and fewer pads are short. If a net does not come back whole, once more with the rooms of the nets that did.
  Not the nets with zones, the pairs or the nets with a length to keep.
- *guide 0.0.26 and 0.0.27* (D124) tell a net that has no way on the board once: the search for a net of two pads with
  no route on the board is for the net whole and says whether it has a way, a search across every route on
  the finest lattice that finds none has said so for the run, and a search that may cross routes looks from
  its other end before it has the whole board. Such a net has no more turns (cynthion's eight at its net
  ties: 9 s in each pass on the finer lattice for one of them, and the first run 21 s for 42 s), guide tells
  it (`Routed::apart`) and the runs after the first are not for it; its open connection still says what
  shuts the pad in where the board's own does. Each net is routed again where the others are before what
  the routes cut off their fills is joined again, and telling the needless vias takes a seventh of the
  time (a piece of fill answers its first points by its edges). *guide 0.0.27* has the order of 0.0.25 for
  those two in a run with a budget of nodes, where 0.0.26 left more open (D124, "Budgets of nodes").
- *guide 0.0.28 and 0.0.29* (D125) route the nets of a row of pads of fine pitch that is in a rule area with a
  clearance of its own no wider than the row's pitch has room
  for two of: the pitch of a pad and the nearest pad of another net, less the net's own clearance and what a
  grid's points miss between two tracks a pitch apart, which is the native router's rule (`fanout::necks`) and
  gives its widths (tinytapeout-demo: 53 nets at 0.185 mm for their class's 0.22 mm, 22 at 0.235 mm for 0.25 mm,
  9 at 0.16 mm). Until then guide took the widest at which two tracks out of two pads side by side clear each
  other by the clearance of the rule area both are in: in a courtyard with a clearance of its own, wider than
  the board's clearance allows the same two tracks beyond it. Its lattice then has the pitch of those tracks,
  half their width and their clearance, where that is tighter (not in a run with a budget of nodes). Found by
  joining the first round of both routers connection by connection (D125 has the join and the count of the
  differences by their kind). 0.0.28 had the rule for every row, which cost corne-cherry connections at 60 s;
  0.0.29 has it in such an area alone, and routes the eleven other boards as 0.0.27 did.
- *guide 0.0.32* (D128) has the native router's order for the pours, and its planning of a net with a plane,
  as two options that the adapter leaves off. `RouteOptions::legal_pours`: when the rounds of guide's
  negotiation end, the routes of the best round are made legal, cadlab's fill around them is asked for, the
  connections open on that board are counted with the pads cut off, the nets with pads cut off are planned
  again and the rounds go on for them, from the routes of the best round; with more open on the next legal
  board the routes go back to the one before. `RouteOptions::outer_pours`: a net with a plane is planned on
  its pours of the outer layers too, the pads that a pour joins as one terminal. Measured stage by stage and
  at 60 s (below, and D128): the first round's difference is closed by the second, 178 vias on
  tinytapeout-demo for 248, and is gone by the end of the run, where the fill of the routed board already
  takes the needless stubs out; with it glasgow-revD1 has 42 to 71 connections open for 12 to 17 and two
  boards a pad with no spoke. The first alone has 2.5 % more track on two boards for nothing gained.
- *guide 0.0.30* (D126) keeps a pad's last spoke, and keeps a net that it routed again only if every track
  of it is free. *The last spoke*: the room of a spoke has a price (`spoke_cost`, D110), and a price let
  the run for shorter ways lay the tracks of two nets through both rooms of a header's ground pin on
  tinytapeout-demo (J14.5, one order of sixteen with 0.0.29). `RouteOptions::kept_spokes` is a wall: of the
  spokes that the fill makes for a pad on a layer, the last that no route of another net stands in the room
  of is in guide's world as copper of the pad's net, for as long as it is the last, from when the nets are
  routed a net at a time. The adapter sets it in the two runs that move routes that are otherwise final,
  the run for shorter ways and the step for the spokes (`KEPT_SPOKES`, and in no run that routes: D126
  has what that cost). Both are also held to the pads by the adapter: a shorter way that leaves a pad that
  had a neck with none has the nets at that pad back as they were, and the step for the spokes is kept by
  how far the board is from its relief, the pads with no spoke first (`relief_score`), tries as many rooms
  for a pad as it lacks spokes, and goes on from each try that it keeps, with half the rooms where a try
  that came back whole gained nothing. *The routes kept across a track*: guide 0.0.25's pass that routes
  each net again where the others are told whether the new route is free by counting the tracks that
  `NetCopper::free_in` keeps of it, which cuts a track where something is in its way and joins what is in
  line; a route across another net's track could be as many tracks as before, and was kept. cadlab's check
  then took out every piece of copper that touched it (tinytapeout-demo at 150 million nodes: 120, 107 and
  323 tracks and vias in three orders of eight; at 60 s 62 and 23 in two of eight), which the later runs
  joined up again. 0.0.30 asks the world of every track and via, and the check takes nothing out.
- *guide 0.0.31* (D127) has three things in its run for shorter ways, and one for every run. *A pour left
  whole*: a pad of a net with a pour on its own layer that the routes of other nets cut off the pour is joined
  to it again by a stub with a via; for each such stub guide now asks its cells whose routes are round the
  pad's piece of the pour and where the fill would join the pad without one, two or three of those nets,
  keeps that way free and routes them again round it, and takes the stub out if they have no more vias, are
  not much longer and cut no other pad off (`Stats::uncut`; buspirate-flash-sop's fifth via). *A run's end at
  a pad*: guide's searches end at a pad's centre and step aside for a pad beside their line before they are
  at it; each run's end is aimed at where on the pad the run comes to it. *A via at 2 mm in a run searched for
  again*, for 3 mm, at which a way round a row of pads cost as much as the way with a via (buspirate-rs232).
  And *what the fills are clear of*: the fills that the adapter gives are cadlab's own of the board it gives
  with them, and guide measured each against every pad and track of that board in every call, a second a
  call on a board of four layers; `Zones::fills_clear_of` says so, and guide measures them against what its
  run adds alone. In the adapter: *a track ends at the first copper of its pad*, half its width before where
  its middle leaves the pad (`pad_ends`), where D118 moved an end back only while its whole round end stayed
  inside the pad and left a track that runs along a pad beside its middle line as long as it was; and *a run
  that moves routes, ends on its own and leaves more open than it was given is the last*, where a second in
  another order came back the same (corne-cherry, two orders of four).
- *The time of the step for the spokes is kept from the first run on* (D124). Where the board as the first
  run leaves it has pads short of their spokes with routes in a spoke's room, every run after the first ends
  in time for the step, as the run for shorter ways did (D122); the step that takes out the tracks between
  pads that a pour joins waits for it where there is not the time for both, and is made after it. And no
  run is begun with less than twice what the first needed after it.
- *guide 0.0.25* (D123) routes each net again where the others are once every route is where it is, by its
  negotiation's own search: a net claims on the grid what its copper keeps others away from wherever it lies,
  copper off the grid by as much more as a track on it may pass nearer between two of its points, and what the
  search finds is kept only if the exact check allows all of it. First for fewer vias alone, then for a way
  as short with as many; on the negotiation's grid as the first run ends, and at the pitch in the run for
  shorter ways. In that run its cleanup lays each run of tracks round the corners of what is beside it, off
  the lattice, where that is shorter: the native router's gridless pass. In its negotiation a plane of another
  net costs a track nothing, as in the native router's, and at twice the pitch it has the native router's
  forty rounds for a hundred. And a pass on the finer lattice that took no net out is the last.
- *No run that moves routes after a first run that moved none and found no way* (D123): the first run that
  ends on its own having taken no net out and moved none aside has said, of every net whose own search found
  no way, what a run that moves routes would say (sweep-v2.2: 39 to 41 s of its minute for 58 s).
- *guide 0.0.24* (D122) charges a net with a pour forty steps for a via in its negotiation at the first pitch,
  the native router's price in its pass for fewer vias, and eight times the passes' price in the stubs that take
  a pad back to a fill on a board of two layers: there the pour is on the pad's own layer, and a track along it
  does what a via to the other side does (corne-cherry's two grounds 92 to 111 vias for 130 to 157, the native
  router's 65 to 71; lumenpnp-ringlight's ground one via for two). And of the two octilinear ways between two
  points its cleanup lays the one with less of its length outside the pads of the net.
- *The runs that move routes end sooner where one has said all there is to say* (D122). After two that gain
  nothing, as before (D117); after one that ended on its own with every open net one whose own search found no
  way and that took no net out and moved none aside, which no other order of the nets changes; and after one
  that was kept and found no way of its own for any net the board then still has open (cynthion: its eight
  nets at the net ties, after the run that closes a ninth; two more runs of 7 s each said the same).
- *The first step that takes needless vias out is made whatever the time*, where a fill is short (3 % of the
  budget at most): on a machine under load it was the step that the runs' late return left out (D121).
- *The run for shorter ways leaves the spoke step its time* (D122): where the board as the runs leave it has
  pads short of their spokes with routes in a spoke's room, the deadline of that run is earlier by what the
  step asks for before it begins.
- *guide 0.0.23* (D121) begins a negotiation at its first pitch that does not end again on finer grids (two
  thirds, a half and a third of the pitch, in several orders), which is what the native router does with its
  whole run (`finer_grid`), and at twice the pitch a net with a plane pays five steps for a via where the
  others pay ten.
- *guide 0.0.22* (D118) routes the nets of a round of its negotiation eight at a time on the adapter's threads,
  with the same routes on any number, and plans the vias of the balls deep in an array whose nets' own via fits.
- *No fill at a run's end* (D117). guide asks its host for a fill at the end of each run, to tell the stubs
  that the fill makes needless; the adapter keeps those on the board until its runs are over and tells them
  again by the fill of the board as it is then, so the fill bought nothing: 0.8 s a run on tinytapeout-demo,
  3 s on glasgow-revD1. It is refused.
- *What the board keeps apart* (guide 0.0.16, D110). A net with pads that the board's own copper, keep-outs,
  holes or edge keep apart on every layer (two halves of a board that only the schematic joins) is looked for
  once by guide on the board with none of its routes, and has no more turns once it has its other connections.
  The first run's word for it is in the report, and the net is in no later run.
- *The count is the board's* (D80). Completion is read from the board with its pours filled again: failed
  connections are the ratsnest lines the board has left for the nets in scope, pads the routing cut off from a
  pour included; routed ones are counted per island of the result (an island holding `k` of the planned islands
  stands for `k − 1` connections), because guide joins a net as a tree of its own, not along the planned lines.
- *Why a connection is open.* guide reports what its result leaves apart, connection by connection, with the pad
  left out and the best-known cause (`Routed::open`). Each failed connection of the board takes the report of
  guide's connection nearest to it (the one that ends at one of its pads, else one whose pad is in the copper at
  one of its ends, else, for a cause that is the net's and not one pad's, any of the net) as its `reason`, `at`,
  `layer`, `subjects` and `hints`:

  | guide's cause | reason | subjects, hints |
  |---|---|---|
  | `Enclosed { by }` | `U1.50 is shut in by pad U1.49, pad U1.51, the routing of net +3V3: no track of net +1V1 can leave it` | the net, and each blocker: the other net, the pad, `track#n` / `via#n`, `keepout:<name>`, the copper drawing; hints by blocker (rip the net, except the net from the keep-out, the board edge, move the parts) |
  | `NoPath` | `guide found no way from <pad> to <pad> for net N; <pad> is not shut in nearby` (also after a run that was stopped: since guide 0.0.5 the pad's surroundings are looked at then too, and a pad that is shut in says so) | the net; the native router, which can move other nets |
  | `OutOfBudget` | `a search of guide for net N reached its node limit before it joined …` | the net; the native router |
  | `Stopped`, the net not in `Routed::tried` | `the time budget ended (the work budget was spent) before guide got to net N: <pad> is apart from <pad>` | the net; a larger `budget` or `work_budget`, fewer nets at a time |
  | `Stopped`, the net in `Routed::tried` | `the time budget ended (the work budget was spent) before guide had routed net N to <pad> again: the net had its turn, and was without a way there when the run stopped` (another net's route took it, or its search was the one cut short) | the net; a larger `budget` or `work_budget`, the native router |
  | `NoLayer` | `<pad> and <pad> share no copper layer net N may be routed on, and the net has no via` | the net; a via or a layer for both pads |
  | `Apart`, from the first run | `the board keeps <pad> apart from <pad>: with no other net routed, no track or via of net N gets from one to the other on any layer (its copper, keep-outs, holes and edges are between them)` | the net; join the two parts of the board that the net is on, or change the keep-out or cutout between them. No hint to the native router. From a later run, where the routes of the runs before are copper of the board, it reads as `NoPath` |
  | `Unknown`, and anything newer | `guide left <pad> apart from <pad> on net N and gives no cause for it` | the net; route the net again with guide, or the native router. None on the twelve corpus boards at 60 s with guide 0.0.4, whose pours between a few pads are no longer planes (D91) |

  Before these: copper of the net that the verification took out (`guide's routing of net N broke a rule and was
  taken out (<code>: <message>)`). After them, where guide reported nothing for the net: a pad the refilled pour
  does not reach (`<pad> is not joined to the pour of net N`).
- *Stats* in the native shape: `connections`, `routed`, `failed`, `completion`, `tracks`, `vias`, `length`,
  `grid` (guide's lattice pitch, half the smallest width plus clearance of the routed nets, or `grid`),
  `vias_removed` (guide's cleanup), `drc_removed`, `fanout_vias` (the vias of guide's escapes, from
  `Routed::via_kinds`), `work` (guide's search nodes, `Stats::work`: the unit of `work_budget` with this
  router, not cadlab's work units), `budget_exhausted` when a budget stopped the run. Pairs are reported as the
  native router reports them, measured on the routed board (`lengths::coupling`).

**Which operations.**

| operation | with `router: guide` |
|---|---|
| `route.all` | routes every open connection |
| `route.nets` | routes the listed nets; other nets' copper is fixed. `shove` (the native push-and-shove of leftovers) does not run |
| `route.connection` | refused: `route.guide_unsupported` (subject `option:router`); guide routes whole nets and has no push-and-shove |
| `route.track`, `route.fanout`, `route.diffpair`, `route.tune`, `route.rip`, `route.status`, `route.import_ses` | take no `router`: always native. They work on a board guide routed as on any other |

**Budgets, cancellation, progress** (guide's `pcb::route_with` and `RunControl`).

- *`work_budget`* is guide's `RouteOptions::work_budget`, in search nodes: the run stops when it is spent and
  returns what it has routed, the same result for the same budget on any machine. A second run draws on what
  the first left. The verification and the fills cost nothing in this unit.
- *The time budget* (`budget`, `budget_ms`, 60 s by default): guide's first run has a deadline at 0.7 of it.
  The verification and the refill that follow are timed, and the second run's deadline is the end of the budget
  less one and a half times that and half a second for the report; a second run that would have less than a
  second is not started. Within a run guide shares its time out itself (0.0.5): routing ends at 0.92 of what is
  left when it begins (three quarters with zones to join, which cadlab turns off), cleanup and the stubs to
  what was cut off the fills have the rest, and every phase looks at the deadline, the plan before
  routing included; a run whose routing ended for its share says that it was stopped, though it comes back
  before its deadline. It makes no fill at the end and joins no zone's pieces (`RouteOptions::fills`,
  `RouteOptions::join`, `RunControl::fill_on_stop`). No call on the twelve corpus boards took longer than its
  60 s (D93, D95, D96, D97). The share was 0.45 until guide 0.0.7 (D95, D96; half before, D91, D93): over three
  orders of the nets the twelve boards had 901 open with two fifths, 890 with 0.45, 971 with half and 1189
  with 0.7, glasgow-revD1 better the sooner its second run started (432, 454, 550, 821) and the eleven other
  boards the longer the first had (469, 436, 421, 368). With guide 0.0.8 glasgow-revD1 is the same either way
  (465 with 0.45, 422 with 0.7) and 0.7 is measured (D97): 917 with 0.45 and 783 with 0.7, and with the release's
  source before it 863, 786 with 0.6, 781 and 821 with 0.8.
- *A run with a budget is not the run without one* (guide 0.0.4): with a work budget or a deadline guide routes
  for the most connections soonest, whether the budget stops it or not, so its result differs from that of a
  run with neither, however large the budget. Each is the same on every run: with a work budget on any machine,
  with a time budget as long as the budget does not cut it.
- Either way the result is verified and counted like any other, `stats.budget_exhausted` is set, the command
  says `route.budget_cut` for a time budget, and each connection left says which budget ended.
- *Cancellation* is looked at in guide's progress callback, four times a second: the run stops there and the
  command ends `cancelled` with the board unchanged.
- *Progress* is guide's: `guide: <phase>, <n> of <m> nets, <k> waiting` with the connections made and to make
  as the count, at every change of phase (escapes and planes, global routing, routing, routing on the finer
  lattice, settling what is open, cleanup, pairs, zones, lengths, report) and four times a second within one; then guide's summary of
  the run, `verifying`, `reporting`, `done`.

**Options.** `pairs: false` routes a pair's nets uncoupled; `grid` sets guide's lattice pitch; `layers` keeps
the tracks to those layers; `seed` (other than 0) is another order of the nets. `effort`, `fanout`, `neck`,
`any_angle`, `gridless`, `arcs`, `arc_radius` and `shove` have no equivalent and are named in one warning,
`route.guide_option_ignored` (of `neck`, `false` still leaves every track at its net's width where it meets a
narrower pad, D121). guide uses one thread: the same board, options, seed and budget give the same
result on every run unless the time budget cut it (`guide_is_identical_for_any_thread_count`).

**What guide cannot express**, passed the stricter way and reported (a warning with subjects and a hint):

- `route.guide_custom_rules`: custom rules with a clearance that can hold a new track or via and are not an
  area's (D61, D96): for tracks alone or vias alone, by containment (`in_area`), with both positional terms,
  between two different scopes (`with`), and `disallow` rules. guide gets the class clearances, the pads' own
  and the areas'; copper that breaks one of these rules is taken out by the verification. A rule for the pads
  of a footprint, or for kinds that are neither track nor via, is given whole and is not named.

Not passed at all: a custom rule's track width, and a narrower width for a net that does not leave its pads
(guide necks such a net whole itself, `RouteOptions::neck_down`; a track wider than the pad it meets is laid
narrower there afterwards, D121), smaller vias under BGAs (a
net has one via), a zone's spoke angle relative to each pad (the fills are cadlab's, so it changes nothing).
A backend error (geometry guide rejects, named by the board's item where guide names one, `RouteError::Item`; or
a panic, which is caught) is `route.guide_failed` and leaves the board unchanged.

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
takes the budgets, `layers`, `seed`, `pairs` and `grid` of them ("guide as a backend", above).

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
With `CADLAB_ORACLE_FREEROUTING=/path/freerouting.jar` (and `CADLAB_ORACLE_JAVA` or `JAVA_HOME` for the Java it
needs: 25 for freerouting 2.5) every board but
`big-4l` is also routed by freerouting (external process on the exported DSN, `-mp 20 -mt 1`; the time includes
the JVM start and file exchange) and imported through `route.import_ses`, then measured the same way. In the
corpus mode `CADLAB_BENCH_ROUTER=freerouting` does the same for the corpus boards, under freerouting's own time
limit ("freerouting on the corpus", at the end of this file).
`CADLAB_ROUTE_RENDER=<dir>` renders cadlab's results; `CADLAB_BENCH_ROUTER=grid|gridless|auto` picks the search,
`CADLAB_BENCH_ROUTER=guide` the guide backend (here and in the corpus mode, under the same budgets; without
`CADLAB_BENCH_SEED` it gets no seed, its own default), and `CADLAB_BENCH_SEED=<n>` the seed.
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
of 201 M between 1 or 2 threads and 4 or 8, which the version before does not show there (not diagnosed in D84; since found in polyclip 0.0.6, not in the
router: one difference of the gridless pass's free space, 2872 clearance rings from a 43 × 33 mm region, comes out
with 89 polygons and 6 % less area on 4 threads or more against 92 on 1 to 3, because the sweep's status keeps a
block that a splice emptied while its own block split, and whether a sweep meets that depends on the bands polyclip
cuts it into per thread; the net's rooms and their work follow. With that fixed in polyclip the run is the same on
1, 2, 4 and 8 threads, work included; until cadlab depends on such a release, results of boards with booleans of
2^15 edges or more can differ between fewer than 4 threads and more).
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

**Native router and guide 0.0.2, D85** (the adapter and guide as they were then; what they are now is measured
under "Native router and guide 0.0.3, D88", "Native router and guide 0.0.4, D91", "Native router and guide
0.0.5, D93", "Native router and guide 0.0.6, D95", "Native router and guide 0.0.7, D96", "Native router and
guide 0.0.8, D97", "Native router and guide 0.0.10, D101" and "Native router and guide 0.0.11, D101" below; the shared 64-core
Linux machine).
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

**Native router and guide 0.0.3, D88** (guide's master before its 0.0.3 release, with run control, the host's
board model and its searches from both ends; the adapter of "guide as a backend" above; the same machine, shared).
The same boards and the same count as under D85: open connections on the routed board with its pours filled again
by cadlab (`drc.unrouted`), DRC errors other than unrouted, vias, track length, wall time of the routing call.

- *guide* now runs under the budgets a user gets: the generated boards with the bench's 120 s time budget (never
  reached), the corpus with the default 60 s, with 300 s, and for the three boards 300 s still stops, 1200 s
  (`CADLAB_BENCH_BUDGET_MS`). No seed (its own default: nets shortest first), one thread, three runs sharing the
  machine at a time. guide's deadline is four fifths of the budget.
- *Native*: seed 1 again on this build, 3 threads: the generated boards (the same copper as D85's seed 1 to
  the via and the tenth of a millimetre) and the corpus at the default 60 s time budget, which is what guide's
  60 s column compares with. The column "200 M work" is D85's (seeds 1 / 2 / 3, 4 threads, 56 to 180 s; the
  native router is unchanged since).
- Commands: `CADLAB_BENCH_ROUTER=guide route_bench <board>`, `CADLAB_BENCH_ROUTER=guide
  [CADLAB_BENCH_BUDGET_MS=300000] route_bench --corpus <board>`, `CADLAB_BENCH_SEED=1 route_bench [--corpus]
  <board>`; `CADLAB_BENCH_VERBOSE=1` lists each open connection with its reason.

| board | connections | native, seed 1: vias, length, time | guide: open | vias | length | time | guide 0.0.2 (D85): open, time |
|---|---|---|---|---|---|---|---|
| attiny-2l | 22 | 4, 104.1 mm, 0.03 s | 0 | 0 | 127.9 mm | 0.02 s | 0, 0.04 s |
| soic24-2l | 184 | 141, 1690.6 mm, 0.4 s | 0 | 159 | 2070.1 mm | 1.3 s | 0, 1.8 s |
| stm32-2l | 109 | 58, 970.4 mm, 0.6 s | 0 | 101 | 1108.9 mm | 15.1 s | 0, 81.8 s |
| stm32-4l | 109 | 54, 902.1 mm, 1.2 s | 0 | 55 | 949.2 mm | 1.5 s | 0, 11.0 s |
| qfp-qfn-4l | 155 | 128, 1934.5 mm, 3.8 s | 0 | 185 | 1933.3 mm | 8.1 s | 0, 47.5 s |
| fine-4l | 100 | 103, 818.8 mm, 1.0 s | 0 | 109 | 958.5 mm | 6.5 s | 0, 137.5 s |
| bga144-4l | 141 | 154, 2211.2 mm, 6.8 s | 0 | 233 | 2517.0 mm | 36.1 s | 14, 127.2 s |

The native router has no open connection on any of the seven, and zero DRC errors; so has guide now, with no via
in an SMD pad (`vias_in_pads` off: 0 of 842 vias by the box test).

Corpus, open connections and the time of the routing call; *cut* marks a run its time budget stopped:

| board | layers | native, 60 s | native, 200 M work (D85) | guide, 60 s | guide, 300 s | guide, 1200 s | guide 0.0.2 (D85) |
|---|---|---|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0.4 s | 0 | 0, 2.3 s | 0, 2.4 s | | 0, 2.8 s |
| buspirate-rs232 | 2 | 0, 0.4 s | 0 | 0, 1.6 s | 0, 1.7 s | | 0, 0.5 s |
| lumenpnp-ringlight | 2 | 0, 0.4 s | 0 | 0, 0.9 s | 0, 0.9 s | | 0, 0.7 s |
| nrfmicro | 2 | 0, 11.9 s | 0 | 13, 8.9 s | 13, 12.1 s | | 15, 69.5 s |
| sweep-v2.2 | 2 | 3, 54.9 s | 3 | 6, 57.1 s cut | 6, 107.1 s | | 8, 67.4 s |
| corne-cherry | 2 | 1, 31.7 s | 2 / 5 / 2 | 91, 13.2 s | 91, 15.8 s | | 95, 270.3 s |
| lumenpnp-mobo | 4 | 10, 62.4 s cut | 8 / 14 / 14 | 71, 65.6 s cut | 18, 166.2 s | | 21, 558.7 s |
| buspirate5-rev10 | 4 | 2, 30.0 s | 2 / 4 / 1 | 160, 53.0 s cut | 200, 245.1 s cut | 11, 355.9 s | 26, 2351 s |
| cynthion | 4 | 27, 61.5 s cut | 30 / 34 / 31 | 82, 60.0 s cut | 44, 255.0 s cut | 25, 351.7 s | 28, 1560 s |
| glasgow-revC3 | 4 | 35, 60.5 s cut | 35 / 35 / 34 | 346, 49.5 s cut | 71, 206.0 s | | no result |
| tinytapeout-demo | 4 | 8, 61.3 s | 25 / 10 / 13 | 121, 54.1 s cut | 116, 70.4 s | | no result at 1200 s |
| glasgow-revD1 | 6 | no result (below) | 122 / 131 / 124 | 791, 66.8 s cut | 657, 262.8 s cut | 311, 982.7 s cut | not run |

Every result of either router has zero DRC errors other than unrouted. Vias and length of guide's runs that
ended on their own: nrfmicro 33, 428.0 mm; sweep-v2.2 44, 2230.7 mm; corne-cherry 289, 3835.5 mm; lumenpnp-mobo
742, 6531.7 mm; buspirate5-rev10 530, 3761.7 mm; cynthion 579, 3635.5 mm; glasgow-revC3 408, 3205.6 mm;
tinytapeout-demo 294, 3336.0 mm. The native router's 60 s run of glasgow-revD1 ended in a panic of its own
(`src/router/engine.rs:1540`, `pour_targets`: a pour index out of range; seed 1, 3 threads, the time budget),
which the work-budget runs of D84 and D85 do not meet; it is not touched here.

What the numbers say:

- *Generated boards.* guide now completes all seven (bga144-4l had 14 open), in 0.02 to 36 s where 0.0.2 took
  0.04 to 138 s: 0.7 to 26 times the native router's time (stm32-2l 15.1 s against 0.6, bga144-4l 36.1 against
  6.8), with 2 to 74 % more vias on six boards and 0 to 23 % more track.
- *Corpus, left to finish.* guide ends on its own on eleven of the twelve boards, in 1 to 356 s (0.0.2: seven
  within 1200 s). On those eleven it leaves 351 connections open; the native router leaves 86 at its default
  60 s (seed 1) and 98 to 105 at 200 M work. Level on the three small boards; ahead on cynthion (25 against 27
  to 34, in 352 s against 62 to 100); behind on the other seven: nrfmicro 13 against 0, sweep-v2.2 6 against 3,
  buspirate5-rev10 11 against 1 to 4, lumenpnp-mobo 18 against 8 to 14, glasgow-revC3 71 against 34 to 35,
  corne-cherry 91 against 1 to 5, tinytapeout-demo 116 against 8 to 25. glasgow-revD1 does not end in 16
  minutes (311 open when cut; native 122 to 131 in 96 to 180 s).
- *What is open, by guide's cause* (`Routed::open`, now in every failed connection's reason): 281 of the 351
  are pads shut in, 202 of them by nothing but the neighbouring pads of their own footprint: the RP2350 and the
  0.5 mm connector of tinytapeout-demo (99 of 116), the BGA of glasgow-revC3 (57 of 71), the RP2040s of
  corne-cherry (40 of 91). A track of the net's width does not fit out between the neighbours, and with no via
  in the pad there is no other way: the native router leaves such a pad narrower (neck-down, D84's escapes) or
  fans a ball out with a smaller via, and guide has one width and one via for a net. The rest: 48 "found no
  way", 15 at a search's node limit, 4 taken out by the verification, 3 others.
- *Under a time budget* guide is far behind: at 60 s it is cut on seven boards and leaves 890 connections open
  on the eleven boards where the native router leaves 86. A run that is cut returns the board as it is at
  that moment, and nets that rip-up has taken out and not yet routed again are not on it: buspirate5-rev10 has
  160 open when cut at 48 s, 200 when cut at 240 s (its three supply nets, 163 connections, were out) and 11
  when it ends at 356 s. The native router keeps its best state so far. guide also ends late: after the
  deadline it still fills the zones around what it has (which the adapter drops), 8 s on cynthion, 14 s on
  lumenpnp-mobo and 15 s on glasgow-revD1 at 60 s, so the routing call takes up to 66.8 s of a 60 s budget.
- *Vias in pads allowed* (`CADLAB_BENCH_VIA_IN_PAD=1`, which no command offers): cynthion 8 open in 110 s, the 8
  Kelvin pads of D72, with 336 of 610 vias in SMD pads; corne-cherry 89 and tinytapeout-demo 114, as without:
  their pads need a narrower track, not a via.
- *DRC.* The verification took nothing out on eleven boards. On corne-cherry it takes 45 items out (4
  connections): guide's escapes of `right/D+` and `right/D-`, two long ways out of the USB connector laid on the
  same line on B.Cu (y = 64.775 mm), which guide's own `drc::check` reports as 5 shorts too. Only with
  `vias_in_pads` off: with vias in pads allowed nothing is taken out.
- *Pours.* Planning on cadlab's fills removed the disagreement D85 met (buspirate-rs232's J2.5 needs no second
  run now). What remains is routes cutting a pad off from its pour: one net on buspirate-flash-sop and
  lumenpnp-ringlight, five on lumenpnp-mobo, which the second run joins or reduces (lumenpnp-mobo: 66 s of its
  166 s for those five nets).
- *Determinism.* A run that no time budget cuts gives the same copper every time, and so does a run under a
  work budget (`guide_refuses_warns_and_keeps_its_limits`, `a_run_keeps_its_budgets_and_hears_the_hooks`);
  corne-cherry gave the same result in four runs here.

The default is unchanged (DECISIONS D88).

**Native router and guide 0.0.4, D91** (guide 0.0.4 from crates.io; the corpus at the default 60 s time budget,
which is what a command gives a run; the same machine, shared, three runs at a time, guide on one thread, no
seed). The same count as above: open connections on the routed board with its pours filled again by cadlab,
vias, track length, the wall time of the routing call; *cut* marks a run a deadline stopped.

- *Before*: guide 0.0.4 behind the adapter of D88, unchanged: one run with a deadline at four fifths of the
  budget, guide's own fill at the end, a second run only after a run that ended on its own. Run twice, with the
  same open connections on every board both times.
- *After*: the adapter of this change ("guide as a backend" above): `fills` off, the smallest via as
  `fanout_via`, the first run's deadline at half the budget and a second run on the routed board in what is
  left.
- *Native*: seed 1, 3 threads, the same 60 s, as measured on this machine for guide's own record of 0.0.4
  (guide's `docs/benchmarks.md`, "PCB routing with a budget"); not run again here.
- Commands: `CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_VERBOSE=1 CADLAB_BENCH_BUDGET_MS=60000 route_bench --corpus
  <board>`; `CADLAB_BENCH_TRACE=1` adds the timeline of guide's phases.

| board | layers | native: open, time | guide before: open, vias, length, time | guide after: open, vias, length, time |
|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0.4 s | 0, 7, 496.7 mm, 3.5 s | 0, 7, 496.7 mm, 1.8 s |
| buspirate-rs232 | 2 | 0, 0.5 s | 0, 6, 358.3 mm, 2.1 s | 0, 6, 358.3 mm, 1.1 s |
| lumenpnp-ringlight | 2 | 0, 0.4 s | 0, 1, 209.0 mm, 1.2 s | 0, 1, 209.0 mm, 0.8 s |
| nrfmicro | 2 | 0, 12.7 s | 11, 37, 423.2 mm, 8.5 s | 11, 37, 423.2 mm, 9.0 s |
| sweep-v2.2 | 2 | 3, 53.7 s | 6, 42, 2250.5 mm, 49.7 s cut | 6, 42, 2250.5 mm, 56.9 s cut |
| corne-cherry | 2 | 1, 30.1 s | 90, 459, 5231.1 mm, 49.4 s cut | 54, 502, 5836.2 mm, 58.9 s cut |
| tinytapeout-demo | 4 | 6, 60.8 s | 47, 478, 4817.2 mm, 49.9 s cut | 45, 478, 4821.2 mm, 37.6 s cut |
| buspirate5-rev10 | 4 | 2, 28.8 s | 17, 510, 3398.6 mm, 49.5 s cut | 14, 513, 3423.3 mm, 48.7 s cut |
| lumenpnp-mobo | 4 | 10, 62.6 s cut | 48, 461, 6192.0 mm, 55.6 s cut | 9, 795, 6576.4 mm, 58.3 s cut |
| cynthion | 4 | 28, 61.4 s cut | 32, 580, 3530.9 mm, 52.4 s cut | 32, 580, 3530.9 mm, 62.3 s cut |
| glasgow-revC3 | 4 | 35, 60.5 s cut | 69, 366, 3113.3 mm, 46.2 s | 69, 414, 3140.6 mm, 42.5 s cut |
| glasgow-revD1 | 6 | no result | 394, 789, 5185.7 mm, 51.0 s cut | 349, 875, 6083.8 mm, 58.6 s cut |
| all twelve | | | 714 | 589 |
| the eleven with a native result | | 85 | 320 | 240 |

Zero DRC errors other than unrouted on every run, and nothing taken out by the verification. For scale: guide
0.0.3 behind the same adapter left 1023 open on the twelve (D88, "As released").

What the numbers say:

- *guide 0.0.4 itself* is most of the way from 1023 to 714: a run with a budget routes for the most
  connections soonest, and a pour between a few pads is copper and no plane. Its two new options change little
  at this budget: with `fills` off and the fan-out via alone, guide's own measurement through this adapter has
  728.
- *The second run* is the adapter's part, 714 to 589. With the deadline at four fifths, a cut run left no time
  for it, and it is the second run that joins pads to their pours on cadlab's fill: lumenpnp-mobo's first run
  was cut 18 s into guide's own joining of its zones' pieces with 48 pads off their pours, all 48 of the board's
  open connections; with the first run stopped at 30 s and the second given the rest it has 9, one fewer than
  the native router (10). corne-cherry, whose routing is zones as wide tracks, 90 to 54; glasgow-revD1 394 to
  349 with the second run routing 26 more nets; buspirate5-rev10 17 to 14, tinytapeout-demo 47 to 45. Unchanged:
  the boards the first run is done with before half the budget, or where the second adds nothing that is kept
  (cynthion, glasgow-revC3, sweep-v2.2, nrfmicro and the three small ones).
- *What the half costs.* The first run is cut before its cleanup on lumenpnp-mobo: 795 vias where the run cut
  at 48 s had 461 (and with a budget of 90 s, where the first run ends on its own, 686 with 8 open). A share of three fifths left that board 58
  open, since the second run then ends inside guide's planning of its stubs, which is not stopped half way and
  returns nothing; four fifths with the second run in what remains, 50.
- *What varies.* A run its deadline cuts ends where the clock finds it: in an earlier run of the same code
  cynthion had 34 open and lumenpnp-mobo 12, where the table has 32 and 9.
- *Over the budget.* cynthion's second run spent 27.5 s planning the escapes and stubs of 18 nets, 9 s past
  its deadline, returned nothing, and the call took 62.3 s of 60. The native router's cut runs take 60.5 to
  62.6 s.
- *What is open, by guide's cause*, of the 589: 391 the time budget ended before (343 of them on
  glasgow-revD1), 187 "found no way", 9 pads shut in, 2 at a search's node limit; none without a cause. After
  a run its deadline stopped, guide does not look at whether a pad is shut in, so "found no way" there includes
  the fine-pitch pads of D88 (glasgow-revC3: 68 "found no way" and 1 shut in after a cut run, 66 shut in and 3
  "found no way" when the run ends on its own); the reason says that it was not looked at.
- *Zones as planes.* With every zone a plane (`CopperZone::plane = Some(true)`, guide's rule before 0.0.4):
  corne-cherry 86 open, lumenpnp-mobo 30. Left to guide: 54 and 9 to 12.
- *Against the native router at the same 60 s:* level on the three small boards, one ahead on lumenpnp-mobo
  (9 against 10), behind on seven (nrfmicro 11 against 0, sweep-v2.2 6 against 3, corne-cherry 54 against 1,
  tinytapeout-demo 45 against 6, buspirate5-rev10 14 against 2, cynthion 32 against 28, glasgow-revC3 69
  against 35), and a result on glasgow-revD1 where the native router's 60 s run has none (its panic, D88).

The default is unchanged (DECISIONS D91).

**Native router and guide 0.0.5, D93** (guide 0.0.5 from crates.io; the corpus at the default 60 s time budget;
the same machine, shared with other measurements at the time, three runs of guide at a time on one thread each,
no seed; the native router one run at a time on three threads, seed 1). The same count as above. A run its
budget cuts ends where the clock finds it, so every row of guide was run twice and both counts are given; the
vias, length and time are those of one of the two.

- *Before*: guide 0.0.4 behind the adapter of D91, as it was.
- *After*: guide 0.0.5 behind the adapter of this change: `join` off, the reason of a stopped connection by
  `Routed::tried`, no special wording for "found no way" after a deadline; the first run's half of the budget
  and the choice of the second run's nets as before, both measured again (below).
- *Native*: run here, with the build of *after*.
- *In SMD pads* is now counted by the pad's copper, not by its box (`routebench::vias_in_pads`): none on any
  board, before or after, for either router. The box counted 3 to 7 vias of guide's on four boards (D91), each
  beside a round pad, in the corner of its box.
- Commands: `RAYON_NUM_THREADS=1 CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_VERBOSE=1 CADLAB_BENCH_TRACE=1
  CADLAB_BENCH_BUDGET_MS=60000 route_bench --corpus <board>`; `RAYON_NUM_THREADS=3 CADLAB_BENCH_VERBOSE=1
  CADLAB_BENCH_BUDGET_MS=60000 route_bench --corpus <board>` for the native router.

| board | layers | native: open, time | guide before: open (two runs), vias, length, time | guide after: open (two runs), vias, length, time |
|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0.5 s | 0 and 0, 7, 496.7 mm, 1.8 s | 0 and 0, 7, 496.7 mm, 0.9 s |
| buspirate-rs232 | 2 | 0, 0.4 s | 0 and 0, 6, 358.3 mm, 1.1 s | 0 and 0, 6, 358.3 mm, 0.4 s |
| lumenpnp-ringlight | 2 | 0, 0.4 s | 0 and 0, 1, 209.0 mm, 0.9 s | 0 and 0, 0, 211.9 mm, 0.4 s |
| nrfmicro | 2 | 0, 11.6 s | 11 and 11, 37, 423.2 mm, 9.7 s | 8 and 8, 44, 430.1 mm, 13.7 s |
| sweep-v2.2 | 2 | 3, 56.0 s | 6 and 6, 42, 2250.5 mm, 56.9 s cut | 6 and 6, 42, 2231.0 mm, 54.9 s cut |
| corne-cherry | 2 | 3, 31.7 s | 54 and 54, 502, 5836.2 mm, 58.9 s cut | 54 and 54, 462, 5833.3 mm, 58.5 s cut |
| tinytapeout-demo | 4 | 8, 60.5 s | 50 and 53, 449, 4606.4 mm, 58.7 s cut | 45 and 45, 478, 4821.2 mm, 38.2 s cut |
| buspirate5-rev10 | 4 | 2, 31.5 s | 14 and 14, 513, 3423.3 mm, 48.1 s cut | 16 and 16, 512, 3402.3 mm, 41.9 s cut |
| lumenpnp-mobo | 4 | 15, 62.6 s cut | 59 and 12, 800, 6617.0 mm, 58.0 s cut | 6 and 7, 704, 6570.0 mm, 58.5 s cut |
| cynthion | 4 | 28, 61.4 s cut | 34 and 36, 564, 3445.5 mm, 62.2 s cut | 23 and 23, 594, 3645.1 mm, 57.1 s cut |
| glasgow-revC3 | 4 | 34, 61.9 s cut | 69 and 69, 414, 3140.6 mm, 42.3 s cut | 69 and 69, 418, 3174.2 mm, 47.4 s cut |
| glasgow-revD1 | 6 | 201, 62.3 s cut | 350 and 349, 875, 6083.8 mm, 58.8 s cut | 349 and 344, 872, 6084.4 mm, 58.4 s cut |
| all twelve | | 294 | 647 and 604 | 576 and 572 |
| the eleven without glasgow-revD1 | | 93 | 297 and 255 | 227 and 228 |

Zero DRC errors other than unrouted on every run, nothing taken out by the verification, and no call of guide
over its 60 s after (62.2 and 65.1 s before, on cynthion and lumenpnp-mobo). The native router has a result on
glasgow-revD1 in this run, 201 open; its panic there (D88) did not come up.

What the numbers say:

- *guide 0.0.5 itself* is the difference: behind the adapter of D91 unchanged it has 586 and 564 open on the
  twelve (guide's own record of the release: 567). nrfmicro 11 to 8 is its gates on the finer lattice,
  cynthion 34 to 23 a second run that now has the time to route, since the plan before it keeps the deadline
  and measures cadlab's fills by their parts.
- *The first run's share*, with `join` off: two fifths 572, half 571 and 573 (and 576 and 572 in the table's
  runs), three fifths 577, two thirds 593, three quarters 636. With `join` on: half 586 and 564, two thirds
  587, four fifths 663. Level from two fifths to three fifths, worse above: a second run on a large board
  spends 8 to 13 s on its plan ("escapes and planes") before it routes, and with a quarter of the budget it has
  no time left to route in (glasgow-revD1 399 where it has 344 to 350, lumenpnp-mobo 51 with four fifths where
  it has 6 to 9). Half stays.
- *The second run is still worth having*: one run with nine tenths of the budget leaves 681 (lumenpnp-mobo 51,
  glasgow-revD1 399, corne-cherry 62, and one connection each on buspirate-flash-sop and lumenpnp-ringlight,
  the pad a route cut off from its pour, D88). guide's own joining of zones' pieces does not stand in for it:
  on lumenpnp-mobo a first run that ends on its own says that it has every connection, and cadlab's fill of
  that board leaves 51 pads off their pours.
- *Which nets the second run takes.* As before: after a stopped run, every net the board has open but those
  with a connection whose cause is a search that failed. With `Routed::tried` in place of the causes (every
  net that had its turn left out): 651, and no second run at all on seven boards, because a stopped run has
  given nearly every net its turn (glasgow-revD1: all 561) and the nets open are those that another net's
  route left without a way, which a run on the routed board finds one for (glasgow-revD1 399 against 349,
  corne-cherry 62 against 51 to 54). `tried` is used for what it says: the reason of a connection the stop
  left open tells a net guide never got to from one that had its turn.
- *`join`.* Off or on, the count is the same within what two runs differ by (567 to 576 against 564 and 586),
  as guide's record says. Off, the three small boards take 0.4 to 1.0 s where they took 1.0 to 1.9 s,
  lumenpnp-mobo's first run has the time for its cleanup (682 to 704 vias and 6 or 7 open in five runs at half, 770 and
  9 in three with it on), and a run with a work budget is not stopped by guide's share for joining: with `join`
  on and 20 M nodes, guide 0.0.5 says the work budget was spent after 35 000, 50 000 and 13 000 nodes on
  buspirate-flash-sop, buspirate-rs232 and lumenpnp-ringlight and leaves one connection open on each
  (`CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_WORK=20000000 route_bench --corpus buspirate-rs232` behind the
  adapter of D91; complete and not stopped with 0.0.4, and with `join` off). So it is off, in both runs.
- *What is open, by guide's cause*, of the 576: 368 the time budget ended before, each of a net that had had
  its turn (343 of them on glasgow-revD1), 161 pads shut in, 30 "found no way", 4 at a search's node limit, 13
  pads the refilled pour does not reach; none without a cause. A pad that is shut in now says so after a cut
  run: glasgow-revC3 has 65 shut in and 4 "found no way", where the cut run of 0.0.4 had 1 and 68.
- *Against the native router at the same 60 s:* level on the three small boards, ahead on lumenpnp-mobo (6 and
  7 against 15) and on cynthion (23 against 28), behind on seven: nrfmicro 8 against 0, sweep-v2.2 6 against 3,
  corne-cherry 54 against 3, tinytapeout-demo 45 against 8, buspirate5-rev10 16 against 2, glasgow-revC3 69
  against 34, glasgow-revD1 344 to 349 against 201.

The default is unchanged (DECISIONS D93).

**Native router and guide 0.0.6, D95** (guide 0.0.6 from crates.io; the corpus at the default 60 s time budget;
the same machine, shared; three runs of guide at a time on one thread each, no seed; the native router one run
at a time on three threads, seed 1). The same count as above. Every row of guide was run twice and both counts
are given; the vias, length and time are those of the first of the two.

- *Before*: guide 0.0.5 behind the adapter of D93, master `323ba6b`.
- *After*: guide 0.0.6 behind the adapter of this change: the first run has 0.45 of the budget; after a run
  that its deadline stopped the second run takes every net the board has open, after one that ended the nets
  with zones less `Routed::failed`; `join` off as before, measured again (below).
- *Native*: run here, with the build of *after*. The same boards three at a time on one thread each: 295 and 94
  where the table has 294 and 93.
- Commands: `RAYON_NUM_THREADS=1 CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_VERBOSE=1 CADLAB_BENCH_TRACE=1
  CADLAB_BENCH_BUDGET_MS=60000 route_bench --corpus <board>`; `RAYON_NUM_THREADS=3 CADLAB_BENCH_VERBOSE=1
  CADLAB_BENCH_BUDGET_MS=60000 route_bench --corpus <board>` for the native router.

| board | layers | native: open, vias, length, time | guide before: open (two runs), vias, length, time | guide after: open (two runs), vias, length, time |
|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 4, 460.2 mm, 0.5 s | 0 and 0, 7, 496.7 mm, 1.0 s | 0 and 0, 7, 496.9 mm, 0.9 s |
| buspirate-rs232 | 2 | 0, 10, 280.9 mm, 0.5 s | 0 and 0, 6, 358.3 mm, 0.4 s | 0 and 0, 6, 356.2 mm, 0.8 s |
| lumenpnp-ringlight | 2 | 0, 2, 180.4 mm, 0.4 s | 0 and 0, 0, 211.9 mm, 0.4 s | 0 and 0, 1, 208.7 mm, 0.3 s |
| nrfmicro | 2 | 0, 46, 453.6 mm, 12.5 s | 8 and 8, 44, 430.1 mm, 13.3 s | 7 and 8, 55, 453.3 mm, 24.7 s |
| sweep-v2.2 | 2 | 3, 21, 2160.9 mm, 56.8 s | 6 and 6, 42, 2231.0 mm, 53.5 s cut | 6 and 6, 42, 2250.5 mm, 55.0 s cut |
| corne-cherry | 2 | 3, 483, 8275.6 mm, 33.7 s | 54 and 54, 462, 5833.3 mm, 58.4 s cut | 46 and 49, 581, 7433.1 mm, 57.0 s cut |
| tinytapeout-demo | 4 | 8, 273, 4910.1 mm, 60.3 s | 45 and 42, 478, 4821.2 mm, 38.0 s cut | 39 and 40, 479, 5105.4 mm, 34.6 s cut |
| buspirate5-rev10 | 4 | 2, 313, 3053.2 mm, 31.9 s | 16 and 16, 512, 3402.3 mm, 42.1 s cut | 6 and 6, 437, 3525.8 mm, 44.9 s cut |
| lumenpnp-mobo | 4 | 15, 344, 5432.4 mm, 62.8 s cut | 7 and 7, 682, 6527.0 mm, 55.9 s | 13 and 13, 731, 6401.2 mm, 45.0 s cut |
| cynthion | 4 | 27, 478, 3176.5 mm, 61.8 s cut | 23 and 22, 597, 3654.3 mm, 57.1 s cut | 22 and 26, 645, 3902.5 mm, 56.5 s cut |
| glasgow-revC3 | 4 | 35, 413, 3421.8 mm, 60.2 s cut | 69 and 69, 418, 3174.2 mm, 47.5 s cut | 37 and 37, 587, 4125.6 mm, 56.8 s cut |
| glasgow-revD1 | 6 | 201, 1008, 8784.5 mm, 63.0 s cut | 344 and 344, 873, 6066.2 mm, 58.8 s cut | 177 and 174, 1067, 7837.8 mm, 58.8 s cut |
| all twelve | | 294 | 572 and 568 | 353 and 359 |
| the eleven without glasgow-revD1 | | 93 | 228 and 224 | 176 and 185 |

Zero DRC errors other than unrouted on every run, nothing taken out by the verification, no via in a pad of
its net, and no call of guide over 59.0 s. With `CADLAB_BENCH_WORK=20000000` in place of the time budget the
twelve boards give the same vias and length in two runs (the three small boards complete, the nine others are
stopped by the budget and get no second run).

What the numbers say:

- *guide 0.0.6 itself* is most of the difference: behind the adapter of D93 it has 421 open (guide's own record
  of that: 409 and 412). glasgow-revC3 69 to 37 is its dog-bones for the balls no track gets out, with the
  smallest via the rules allow (`fanout_via`: 54 without it); buspirate5-rev10 16 to 7 and glasgow-revD1 344
  to 232 its cutting and mending of the nets in a route's way; corne-cherry 54 to 47 its rows' tracks side by
  side.
- *Which nets the second run takes* (the first run at half, as it was). `Routed::failed` in place of the
  causes: 420 and 420 against 421, the same nets. With no net left out after a cut run: 401, 411, 411 and
  419; on the eleven boards without glasgow-revD1, whose count moves by six between two runs, 174 to 181
  against 188 and 189. tinytapeout-demo's first run has 26 nets with no way of their own of 30 open, and the
  second run, given them, makes 5 to 7 more connections in 4 s; with them left out it ended 24 s before the
  budget did.
- *The first run's share* (the second run taking every open net): a third 397, two fifths 366 and 369, 0.45
  356 and 350, and 353 and 359 in the table's runs, half 411 and 419, three fifths 499. glasgow-revD1 is 183,
  167 to 177, 171 to 178, 227 to 238 and 321 along that row: its first run has 327 open at 27 s and at 30 s,
  and its second run, once it has planned for 10 s, makes some ten connections a second until its deadline.
  cynthion is 32, 31 to 36, 20 to 26, 21 to 24 and 27: its second run plans for 8 to 15 s. The nine other
  boards are the same from two fifths to half within what two runs differ by. So 0.45, where it was half
  (with `Routed::failed` leaving nets out, the row was 411, 458, 420, 498, and 513 and 512 for two thirds and
  three quarters; corne-cherry 54, 49, 47, 47, 65, 72 there: the longer its first run, the more nets end with
  no way of their own, 4 at 30 s and 21 at 44 s).
- *The second run is still worth having*: one run with nine tenths of the budget leaves 598 (lumenpnp-mobo 59,
  corne-cherry 98 of which 33 are pads off their pours, lumenpnp-ringlight 9, one each on buspirate-flash-sop
  and buspirate-rs232). cynthion alone is better with one run, 16.
- *`join`.* On (the first run at half): 432 against 420, and 0.9 to 1.8 s on the three small boards for 0.4 to
  0.9 s. guide 0.0.6 no longer stops a run with a work budget for it (D93): with `join` on and 20 M nodes
  buspirate-rs232, buspirate-flash-sop and lumenpnp-ringlight complete after 174 935, 94 587 and 11 441 nodes.
  It stays off for the count and the time.
- *Vias and track.* guide 0.0.6 makes its connections with more copper than 0.0.5: corne-cherry 581 vias and
  7433 mm for 462 and 5833, glasgow-revC3 587 and 4126 mm for 418 and 3174, glasgow-revD1 1067 and 7838 mm for
  873 and 6066 (and 167 more connections); buspirate5-rev10 has fewer vias (437 for 512), nrfmicro more (55
  for 44). Beside the native router it has twice the vias on lumenpnp-mobo (731 against 344) and nearly that
  on tinytapeout-demo (479 against 273), a fifth to two fifths more on corne-cherry, cynthion, buspirate5-rev10
  and glasgow-revC3, and 6 % more on glasgow-revD1; less track on corne-cherry, where it leaves 43 more
  connections open, and on glasgow-revD1, where it leaves 24 fewer, and 4 to 23 % more on the five others.
- *lumenpnp-mobo* is the one board that is worse, 13 for 7. With no seed and the first run at half, that run
  searches for one net, `Microcontroller/PE10`, from 9.4 s to 27.8 s, 4.8 s past routing's share, and is cut
  2.2 s into its cleanup; with seeds 1 and 2 it ends on its own and the board has 6 and 6 open (guide 0.0.5: 7
  and 8). Without `fanout_via` and no seed the first run ends at 18.7 s and 7 are open (4 and 10 with the
  seeds), though the result with it has no escape via. `neck_down` off: 13; `join` on: 14; the first run at
  0.45: 13, at three fifths to three quarters: 10.
- *What is open, by guide's cause*, of the 353: 243 pads shut in (124 of them on glasgow-revD1), 50 the time
  budget ended before, each of a net that had had its turn (33 of them on glasgow-revD1), 28 at a search's
  node limit, 25 "found no way", 7 pads the refilled pour does not reach.
- *Against the native router at the same 60 s:* level on the three small boards, ahead on glasgow-revD1 (177
  and 174 against 201), on lumenpnp-mobo (13 against 15) and on cynthion (22 and 26 against 27), behind on six:
  glasgow-revC3 37 against 35, buspirate5-rev10 6 against 2, sweep-v2.2 6 against 3, nrfmicro 7 and 8 against
  0, tinytapeout-demo 39 and 40 against 8, corne-cherry 46 and 49 against 3.

The default is unchanged (DECISIONS D95).

**Native router and guide 0.0.7, D96** (guide 0.0.7 from crates.io; the corpus at the default 60 s time budget;
the same machine, shared, with a load of 7 to 12 from other work; three runs of guide at a time on one thread
each; the native router one run at a time on three threads, seed 1). The same count as above. Every row of
guide was run in three orders of its nets, no seed and seeds 2 and 3, and the three counts are given in that
order; the vias, length and time are those of the first.

- *0.0.6*: guide 0.0.6 behind the adapter of D95, master `c85fc3c`, run again here.
- *0.0.7*: guide 0.0.7 behind the same adapter.
- *After*: guide 0.0.7 behind the adapter of this change: a courtyard's or an area's clearance rule goes over
  as a rule area when its kinds take in tracks and vias, raised to the later rules that hold copper in it, and
  the last rule holding a pad is the pad's own clearance. Only tinytapeout-demo has custom rules; the other
  boards differ from *0.0.7* by what two runs differ by.
- Commands: `RAYON_NUM_THREADS=1 CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_VERBOSE=1 CADLAB_BENCH_TRACE=1
  CADLAB_BENCH_BUDGET_MS=60000 [CADLAB_BENCH_SEED=2|3] route_bench --corpus <board>`; `RAYON_NUM_THREADS=3
  CADLAB_BENCH_VERBOSE=1 CADLAB_BENCH_BUDGET_MS=60000 route_bench --corpus <board>` for the native router.

| board | layers | native: open, vias, length, time | guide 0.0.6: open (three orders); vias, length, time | guide 0.0.7: open | guide after: open (three orders); vias, length, time |
|---|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 4, 460.2 mm, 0.4 s | 0, 0, 0; 7, 496.9 mm, 0.9 s | 0, 0, 0 | 0, 0, 0; 7, 496.9 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 10, 280.9 mm, 0.4 s | 0, 0, 0; 6, 356.2 mm, 0.8 s | 0, 0, 0 | 0, 0, 0; 6, 356.2 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 2, 180.4 mm, 0.4 s | 0, 0, 0; 1, 208.7 mm, 0.4 s | 0, 0, 0 | 0, 0, 0; 1, 208.7 mm, 0.3 s |
| nrfmicro | 2 | 0, 46, 453.6 mm, 11.9 s | 8, 5, 5; 50, 493.7 mm, 25.3 s cut | 8, 5, 5 | 8, 5, 5; 48, 496.5 mm, 25.3 s cut |
| sweep-v2.2 | 2 | 3, 21, 2160.9 mm, 52.6 s | 6, 7, 6; 42, 2250.5 mm, 54.9 s cut | 6, 7, 6 | 6, 7, 6; 42, 2231.0 mm, 55.0 s cut |
| corne-cherry | 2 | 3, 483, 8275.6 mm, 30.1 s | 49, 50, 43; 600, 7447.0 mm, 57.0 s cut | 45, 37, 35 | 45, 37, 35; 609, 7605.7 mm, 56.8 s cut |
| tinytapeout-demo | 4 | 6, 274, 4913.3 mm, 61.1 s | 40, 40, 37; 482, 4993.5 mm, 34.4 s cut | 40, 35, 44 | 14, 20, 20; 568, 5560.3 mm, 35.7 s cut |
| buspirate5-rev10 | 4 | 2, 313, 3053.2 mm, 28.4 s | 6, 6, 5; 441, 3539.8 mm, 45.3 s cut | 11, 9, 5 | 11, 6, 5; 477, 3522.7 mm, 35.1 s cut |
| lumenpnp-mobo | 4 | 15, 347, 5423.7 mm, 62.5 s cut | 14, 6, 7; 734, 6404.3 mm, 46.3 s cut | 10, 5, 7 | 10, 5, 7; 734, 6330.2 mm, 38.2 s cut |
| cynthion | 4 | 28, 476, 3177.0 mm, 61.4 s cut | 25, 28, 30; 648, 3956.1 mm, 57.4 s cut | 18, 22, 26 | 19, 22, 32; 612, 3765.4 mm, 43.6 s cut |
| glasgow-revC3 | 4 | 36, 415, 3436.4 mm, 60.7 s cut | 36, 44, 40; 589, 4142.5 mm, 56.8 s cut | 35, 38, 40 | 34, 37, 40; 573, 4060.3 mm, 56.7 s cut |
| glasgow-revD1 | 6 | 192, 1046, 8967.0 mm, 60.7 s cut | 228, 231, 201; 1022, 7240.0 mm, 58.9 s cut | 165, 136, 130 | 168, 136, 150; 1162, 8123.9 mm, 58.8 s cut |
| all twelve | | 285 | 412, 417, 374 | 338, 294, 298 | 315, 275, 300 |
| the eleven without glasgow-revD1 | | 93 | 184, 186, 173 | 173, 158, 168 | 147, 139, 150 |

Zero DRC errors other than unrouted on every run, no via in a pad of its net, and no call of guide over 59.0 s.
The verification took nothing out in the runs of *0.0.7* and *after*; in one run of *0.0.6* (corne-cherry,
seed 2) it took out 43 items of GNDR for a via of the second run on a via of the first.

What the numbers say:

- *What two runs differ by.* The row of 0.0.6 is the adapter D95 recorded at 353 and 359: glasgow-revD1 has
  228 here where it had 177 and 174. A run that its deadline cuts depends on what else the machine does.
  *After* in the first order a second time: 306 for 315, glasgow-revD1 157 for 168, buspirate5-rev10 15 for
  11, tinytapeout-demo 11 for 14. Between orders the twelve boards move by forty.
- *guide 0.0.7 itself*: 412, 417, 374 to 338, 294, 298. glasgow-revD1 228, 231, 201 to 165, 136, 130, whose
  second run starts to search 3.3 s after it is called where it took 10; corne-cherry 49, 50, 43 to 45, 37,
  35; cynthion 25, 28, 30 to 18, 22, 26. buspirate5-rev10 is worse in two orders, 11 and 9 for 6 and 6.
- *The rules*: tinytapeout-demo 40, 35, 44 to 14, 20, 20 (the native router 6). Its areas: the QFN-80's
  courtyard at 0.18 mm on the front (the rule says 0.16 mm, the later rule for the part's pads 0.18 mm), the
  two carrier connectors' at 0.16 mm, the USB connector's at 0.2 mm; 81 pads with 0.18 mm of their own, 116
  with 0.16 mm, 20 with 0.2 mm. What it has open in the first order: 13 pads shut in, nine of them supply pins
  of the QFN and the capacitors beside them, whose nets are 0.25 mm wide and are not routed at the 0.16 mm
  the courtyard's rule allows; one pad off its pour. It has 568 vias for 482, and 5560 mm for 4994.
- *The first run's share* (sums over the three orders; the share read from the environment in a build that is not committed): 0.35
  920, two fifths 901, 0.45 890, half 971, 0.55 955, three fifths 1019, 0.65 1159, 0.7 1189. glasgow-revD1:
  427, 432, 454, 550, 552, 640, 783, 821. The eleven others: 493, 469, 436, 421, 403, 379, 376, 368, of which
  cynthion 90, 73, 73, 70, 51, 47, 42, 40, tinytapeout-demo 58, 52, 54, 46, 41, 40, 41, 41 and corne-cherry
  136, 139, 117, 121, 127, 115, 115, 109. 0.45 stays: the least sum, set by one board against eleven.
- *The second run is still worth having*: one run with nine tenths of the budget leaves 571, 569 and 480
  (lumenpnp-mobo 61, 52, 46; corne-cherry 97, 88, 74; glasgow-revD1 309, 313, 251; 11 or 12 pads off their
  pours on the three small boards). cynthion alone is better with one run: 17, 17, 23.
- *Vias and track.* Beside the native router guide has twice the vias on lumenpnp-mobo (734 against 347),
  tinytapeout-demo (568 against 274) and sweep-v2.2 (42 against 21), a quarter to a half more on corne-cherry,
  cynthion, glasgow-revC3 and buspirate5-rev10, and a ninth more on glasgow-revD1; less track on corne-cherry
  (7606 against 8276 mm, with 42 more connections open) and glasgow-revD1 (8124 against 8967 mm, with 24
  fewer), 3 to 19 % more on the others.
- *What is open, by guide's cause*, of the 315 of the first order: 199 pads shut in, 46 the time budget ended
  before, 34 at a search's node limit, 22 "found no way", 14 pads the refilled pour does not reach.
- *Against the native router at the same 60 s*, three orders against its one run: level on the three small
  boards; ahead in every order on glasgow-revD1 (168, 136, 150 against 192) and lumenpnp-mobo (10, 5, 7
  against 15); within what runs differ by on cynthion (19, 22, 32 against 28) and glasgow-revC3 (34, 37, 40
  against 36); behind in every order on buspirate5-rev10 (11, 6, 5 against 2), sweep-v2.2 (6, 7, 6 against 3),
  nrfmicro (8, 5, 5 against 0), tinytapeout-demo (14, 20, 20 against 6) and corne-cherry (45, 37, 35 against
  3).

The default is unchanged (DECISIONS D96).

**Native router and guide 0.0.8, D97** (guide 0.0.8 and 0.0.9 from crates.io; the corpus at the default 60 s time
budget; the same machine, shared, with a load of 5 to 9 from other work; six runs of guide at a time on one thread
each; the native router one run at a time on three threads, seed 1). The same count as above. Every row of guide was
run in three orders of its nets, no seed and seeds 2 and 3, and the three counts are given in that order; the vias,
length and time are those of the first.

- *0.0.7*: guide 0.0.7 behind the adapter of D96, master `ecf0242`, run again here.
- *0.0.8*: guide 0.0.8 behind the same adapter (the first run with 0.45 of the budget).
- *0.0.8, first run 0.7*: the same with the first run at 0.7 of the budget and nothing else.
- *After*: guide 0.0.9, which is 0.0.8 without the via on a via, behind the adapter of this change: the first run at
  0.7 of the budget and with the nets that have zones among its nets, open or not; `pour_cuts` on and `row_fanout`
  off, as guide has them. guide 0.0.8 behind the same adapter has 276, 253 and 231, and 273, 255 and 235 in a second
  run.
- Commands: `RAYON_NUM_THREADS=1 CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_VERBOSE=1 CADLAB_BENCH_TRACE=1
  CADLAB_BENCH_BUDGET_MS=60000 [CADLAB_BENCH_SEED=2|3] route_bench --corpus <board>`; `RAYON_NUM_THREADS=3
  CADLAB_BENCH_VERBOSE=1 CADLAB_BENCH_BUDGET_MS=60000 route_bench --corpus <board>` for the native router. The two
  middle rows are of a build with the share and the nets of the first run read from the environment, which is not
  committed.

| board | layers | native: open, vias, length, time | guide 0.0.7: open (three orders); vias, length, time | guide 0.0.8: open | guide 0.0.8, first run 0.7: open | guide after: open (three orders); vias, length, time |
|---|---|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 4, 460.2 mm, 0.5 s | 0, 0, 0; 7, 496.9 mm, 0.8 s | 0, 0, 0 | 0, 0, 0 | 0, 0, 0; 7, 496.9 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 10, 280.9 mm, 0.5 s | 0, 0, 0; 6, 356.2 mm, 0.7 s | 0, 0, 0 | 0, 0, 0 | 0, 0, 0; 6, 356.2 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 2, 180.4 mm, 0.4 s | 0, 0, 0; 1, 208.7 mm, 0.3 s | 0, 0, 0 | 0, 0, 0 | 0, 0, 0; 1, 209.6 mm, 0.3 s |
| nrfmicro | 2 | 0, 46, 453.6 mm, 11.0 s | 8, 5, 5; 48, 496.5 mm, 25.3 s cut | 4, 7, 7 | 4, 7, 7 | 4, 7, 9; 62, 499.4 mm, 25.6 s |
| sweep-v2.2 | 2 | 3, 21, 2160.9 mm, 53.3 s | 6, 7, 6; 42, 2231.0 mm, 54.8 s cut | 6, 8, 6 | 6, 7, 6 | 6, 7, 6; 42, 2231.0 mm, 56.1 s cut |
| corne-cherry | 2 | 3, 483, 8275.6 mm, 30.1 s | 45, 37, 42; 609, 7605.7 mm, 56.8 s cut | 41, 36, 43 | 36, 35, 38 | 36, 35, 38; 825, 8435.7 mm, 46.6 s cut |
| tinytapeout-demo | 4 | 6, 274, 4913.3 mm, 60.0 s | 13, 20, 20; 572, 5530.5 mm, 40.1 s cut | 21, 14, 6 | 9, 8, 5 | 9, 8, 5; 605, 5799.4 mm, 48.8 s cut |
| buspirate5-rev10 | 4 | 2, 313, 3053.2 mm, 29.7 s | 5, 9, 7; 436, 3536.4 mm, 45.2 s cut | 9, 6, 7 | 2, 6, 3 | 2, 6, 3; 501, 3815.8 mm, 36.6 s |
| lumenpnp-mobo | 4 | 15, 347, 5423.7 mm, 62.4 s cut | 10, 5, 7; 734, 6330.2 mm, 38.9 s cut | 10, 10, 6 | 9, 10, 6 | 0, 1, 0; 713, 6544.4 mm, 30.5 s |
| cynthion | 4 | 28, 476, 3177.0 mm, 61.3 s cut | 20, 22, 28; 593, 3682.1 mm, 42.8 s cut | 26, 23, 28 | 19, 14, 11 | 19, 14, 19; 631, 3910.6 mm, 57.4 s cut |
| glasgow-revC3 | 4 | 36, 415, 3436.4 mm, 60.6 s cut | 34, 37, 40; 581, 4063.2 mm, 56.8 s cut | 46, 41, 41 | 38, 38, 37 | 35, 36, 34; 541, 3918.7 mm, 57.0 s cut |
| glasgow-revD1 | 6 | 201, 1008, 8784.5 mm, 62.5 s cut | 213, 136, 188; 1098, 7532.8 mm, 58.7 s cut | 167, 157, 141 | 162, 136, 124 | 162, 136, 124; 1085, 7953.6 mm, 58.0 s cut |
| all twelve | | 294 | 354, 278, 343 | 330, 302, 285 | 285, 261, 237 | 273, 250, 238 |
| the eleven without glasgow-revD1 | | 93 | 141, 142, 155 | 163, 145, 144 | 123, 125, 113 | 111, 114, 114 |

Zero DRC errors other than unrouted on every run, no via in a pad of its net, and no call of guide over 58.8 s. The
verification took nothing out in any of these runs, those of 0.0.8 behind the new adapter included.

What the numbers say:

- *What two runs differ by.* The row of 0.0.7 is the adapter D96 recorded at 315, 275 and 300: glasgow-revD1 has
  213, 136 and 188 here where it had 168, 136 and 150, and the same binary had 214, 252 and 163, and 155, 141 and
  128, in two runs of this session two hours apart. A run that its deadline cuts depends on what else the machine
  does. *After* a second time: 272, 250 and 239 for 273, 250 and 238. With 0.0.8 and 0.0.9 the boards that the
  budget cuts repeat nearly to the connection: a pass that ends with fewer connections than its routes made at their
  best goes back to those, and the best is reached before the cut.
- *guide 0.0.8 itself, behind the same adapter*: 354, 278 and 343 to 330, 302 and 285. Nothing in sum, and not the
  same runs: glasgow-revD1's first run has 182 of 1948 connections open at 27 s where 0.0.7's has 325, and its
  second makes 15 where it made 111. tinytapeout-demo 13, 20 and 20 to 21, 14 and 6, 53 to 41: the supply pins of
  its QFN are necked by the courtyard's clearance, and the pads its routes cut off their fills are joined by guide.
- *The first run at 0.7*: 330, 302 and 285 to 285, 261 and 237. glasgow-revD1 167, 157 and 141 to 162, 136 and 124,
  where 0.0.7 with 0.7 has 306, 272 and 243 (measured here two hours before, 1190 on the twelve boards for 851 with
  0.45 in that hour): the board no longer needs its first run short. The eleven others 163, 145 and 144 to 123, 125
  and 113: cynthion 26, 23 and 28 to 19, 14 and 11, corne-cherry 41, 36 and 43 to 36, 35 and 38, buspirate5-rev10 9,
  6 and 7 to 2, 6 and 3. In the uncommitted build of 0.0.8's source, over the three orders: 0.45 863, 0.6 786, 0.7
  781, 0.8 821; `row_fanout` on, 367 on the eleven boards for 358; `pour_cuts` off, 381.
- *The nets with zones in the first run*, with 0.0.8: 285, 261 and 237 to 276, 253 and 231, all of it lumenpnp-mobo,
  9, 10 and 6 to 0, 1 and 0. Its ground had no open connection before routing, as cadlab's fill of the F.Cu pour
  joins all its pads, so the first run was not asked for it; the routes cut the pour, and the second run was asked
  for some 220 stubs to the plane on a board where nothing moves. In the first run guide makes them while the board
  is free: 813 of 813 connections in 21 s for 524 of 524 in 33 s, 713 vias for 748. With 0.45 and these nets: 908
  over the three orders for 760.
- *guide 0.0.9*: 276, 253 and 231 to 273, 250 and 238. A path laid from its far end through a via of its own net,
  where a track of the net ends, is no longer given a via of its own on it (a via on a via, which the verification
  took out with its piece twice in D96's runs).
- *Vias and track.* Beside the native router guide has twice the vias on lumenpnp-mobo (713 against 347),
  tinytapeout-demo (605 against 274) and sweep-v2.2 (42 against 21), 1.3 to 1.7 times on nrfmicro, glasgow-revC3,
  cynthion, buspirate5-rev10 and corne-cherry (825 against 483), and 1.08 times on glasgow-revD1; less track on
  glasgow-revD1 (7954 against 8784 mm, with 39 fewer connections open), 2 to 25 % more on the others.
- *What is open, by guide's cause*, of the 273 of the first order: 175 that the budget ended before, 68 pads shut
  in, 18 "found no way", 10 at a search's node limit, 2 pads the refilled pour does not reach.
- *Against the native router at the same 60 s*, three orders against its one run: level on the three small boards;
  ahead in every order on glasgow-revD1 (162, 136 and 124 against 201), lumenpnp-mobo (0, 1 and 0 against 15) and
  cynthion (19, 14 and 19 against 28); within what runs differ by on tinytapeout-demo (9, 8 and 5 against 6),
  buspirate5-rev10 (2, 6 and 3 against 2) and glasgow-revC3 (35, 36 and 34 against 36); behind in every order on
  nrfmicro (4, 7 and 9 against 0), sweep-v2.2 (6, 7 and 6 against 3) and corne-cherry (36, 35 and 38 against 3).
  273, 250 and 238 against 294 on the twelve boards, 111, 114 and 114 against 93 on the eleven without
  glasgow-revD1.
- *The generated boards* through the adapter of this change with 0.0.9, one run each with the default budget of 120
  s: all eight complete with no DRC error, and the same routes with 0.0.8 (attiny-2l 0 vias in 0.02 s, soic24-2l 159
  in 1.2 s, stm32-2l 72 in 1.4 s, stm32-4l 51 in 1.1 s, qfp-qfn-4l 172 in 4.4 s, fine-4l 105 in 3.1 s, bga144-4l 248
  in 13.6 s, big-4l 1994 vias and 21 789 mm in 70 s).

The default is unchanged (DECISIONS D97).

**Native router and guide 0.0.10, D101** (guide 0.0.9 and 0.0.10 from crates.io; the corpus at the default 60 s
time budget; the same machine, which other work loaded with 68 to 75 runnable threads on its 64 through all of
it, where D97's hour had 5 to 9: a minute of one thread made about half the search nodes it made then, for either
router; six runs of guide at a time on one thread each, the three rows of guide by turns in one queue; the native
router one run at a time on three threads, seed 1, half an hour before). The same count as above, three orders of
the nets for guide (no seed, seeds 2 and 3); the vias, length and time are those of the first.

- *0.0.9*: guide 0.0.9 behind the adapter of D97, master `5a50d3a`.
- *0.0.10*: guide 0.0.10 behind the same adapter, with nothing changed but the crate and the name of its new
  phase: its passes end where they have stopped gaining, and what they leave open is settled.
- *After*: guide 0.0.10 behind the adapter of this change: the rows of fine pitch planned on boards of two layers.
- Commands as for D97.

| board | layers | native: open, vias, length, time | guide 0.0.9: open (three orders); vias, length, time | guide 0.0.10: open | guide after: open (three orders); vias, length, time |
|---|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 4, 460.2 mm, 0.8 s | 0, 0, 0; 7, 496.9 mm, 1.2 s | 0, 0, 0 | 0, 0, 0; 7, 496.9 mm, 2.1 s |
| buspirate-rs232 | 2 | 0, 10, 280.9 mm, 0.7 s | 0, 0, 0; 6, 356.2 mm, 1.1 s | 0, 0, 0 | 0, 0, 0; 6, 356.2 mm, 1.2 s |
| lumenpnp-ringlight | 2 | 0, 2, 180.4 mm, 1.3 s | 0, 0, 0; 1, 209.6 mm, 0.4 s | 0, 0, 0 | 0, 0, 0; 1, 209.6 mm, 0.5 s |
| nrfmicro | 2 | 0, 46, 453.6 mm, 22.0 s | 4, 7, 9; 62, 499.4 mm, 39.1 s cut | 4, 8, 8 | 3, 5, 3; 49, 500.3 mm, 9.7 s |
| sweep-v2.2 | 2 | 3, 21, 2160.9 mm, 63.1 s | 6, 7, 6; 42, 2231.0 mm, 54.9 s cut | 6, 8, 6 | 6, 8, 6; 42, 2231.0 mm, 54.8 s cut |
| corne-cherry | 2 | 3, 483, 8275.6 mm, 60.8 s | 45, 38, 43; 732, 7847.0 mm, 53.8 s cut | 43, 37, 39 | 36, 38, 40; 448, 5816.1 mm, 57.6 s cut |
| tinytapeout-demo | 4 | 20, 240, 4884.2 mm, 64.4 s cut | 26, 14, 7; 565, 5335.2 mm, 58.1 s cut | 26, 14, 7 | 26, 14, 6; 565, 5335.2 mm, 56.1 s cut |
| buspirate5-rev10 | 4 | 2, 313, 3053.2 mm, 61.0 s | 8, 10, 10; 517, 3527.6 mm, 50.6 s cut | 8, 9, 11 | 9, 8, 10; 449, 3526.8 mm, 58.1 s cut |
| lumenpnp-mobo | 4 | 16, 338, 5437.2 mm, 63.9 s cut | 2, 1, 5; 711, 6527.4 mm, 54.7 s | 4, 9, 6 | 4, 8, 5; 704, 6556.6 mm, 47.0 s |
| cynthion | 4 | 33, 478, 3190.0 mm, 64.3 s cut | 34, 30, 41; 552, 3390.9 mm, 49.7 s cut | 34, 30, 41 | 34, 26, 41; 554, 3389.5 mm, 50.3 s cut |
| glasgow-revC3 | 4 | 49, 413, 3492.5 mm, 61.8 s cut | 52, 42, 45; 507, 3604.8 mm, 57.1 s cut | 52, 38, 45 | 52, 36, 45; 507, 3604.8 mm, 58.6 s cut |
| glasgow-revD1 | 6 | 244, 930, 8154.8 mm, 67.1 s cut | 242, 319, 196; 962, 6750.5 mm, 49.0 s cut | 242, 272, 191 | 242, 265, 187; 962, 6750.5 mm, 61.8 s cut |
| all twelve | | 370 | 419, 468, 362 | 419, 425, 354 | 412, 408, 343 |
| the eleven without glasgow-revD1 | | 126 | 177, 149, 166 | 177, 153, 163 | 170, 143, 156 |

Zero DRC errors other than unrouted on every run, no via in a pad of its net, nothing taken out by the
verification. The longest call of guide took 61.4 s with 0.0.9 and 61.8 s with 0.0.10 (58.8 s in D97's hour): the
verification and the refill after a run are timed on the first run and were slower after the second.

The same boards with a budget of work, 40 million of guide's search nodes, which is the same run on any machine
under any load (`CADLAB_BENCH_WORK=40000000`; after a run that a work budget stopped there is no second run):
guide 0.0.9 → the source of 0.0.10 behind the adapter of D97, and → with the rows planned (`row_fanout` read from
the environment in a build that is not committed; the three small boards are complete in each):

| board | layers | open, 0.0.9 → 0.0.10 (three orders) | with `row_fanout` |
|---|---|---|---|
| nrfmicro | 2 | 4, 7, 9 → 4, 8, 8 | 3, 5, 3 |
| sweep-v2.2 | 2 | 6, 7, 6 → 6, 7, 6 | not run |
| corne-cherry | 2 | 58, 57, 54 → 50, 43, 47 | 42, 46, 42 |
| tinytapeout-demo | 4 | 14, 19, 5 → 14, 19, 5 | 18, 16, 9 |
| buspirate5-rev10 | 4 | 2, 6, 5 → 0, 6, 5 | 12, 3, 0 |
| lumenpnp-mobo | 4 | 0, 1, 0 → 0, 0, 1 | not run |
| cynthion | 4 | 25, 17, 25 → 21, 17, 25 | 21, 17, 25 |
| glasgow-revC3 | 4 | 36, 38, 37 → 36, 35, 36 | 34, 36, 39 |
| glasgow-revD1 | 6 | 170, 145, 139 → 170, 145, 139 | not run |
| the nine | | 892 → 853 | |

What the numbers say:

- *The load.* The native router has 370 open on the twelve boards where D97 recorded 294, and guide 0.0.9 behind
  the same adapter 419, 468 and 362 where it had 273, 250 and 238: glasgow-revD1 242, 319 and 196 for 162, 136
  and 124, buspirate5-rev10 8, 10 and 10 for 2, 6 and 3, tinytapeout-demo 26, 14 and 7 for 9, 8 and 5. A run of
  guide on one thread is cut where the clock finds it, and loses more to a loaded machine than three threads
  do. Nothing in this table is to be read against D97's; the rows are of one hour.
- *guide 0.0.10 itself*, behind the same adapter: 419, 468 and 362 to 419, 425 and 354. corne-cherry 45, 38 and 43
  to 43, 37 and 39; glasgow-revD1's second order 319 to 272, which two runs of one build differ by on that board
  in this session (344 and 267 two hours before); the others within a few connections either way. With a budget of
  work the same change is 892 to 853 on the nine boards over the three orders, and no board has more in sum:
  corne-cherry 169 to 140, cynthion 67 to 63, glasgow-revC3 111 to 107, buspirate5-rev10 13 to 11. corne-cherry's
  first pass has 395 of 432 connections after 4 million nodes; 0.0.9 goes on, taking nets out for each other, and
  has 396 at best 23 million nodes later; 0.0.10 ends its passes after 12 million nodes with 397 to 400 and then
  tries 32 to 45 chains in an order, of which 6 to 8 are kept and make 12 to 15 connections or pieces of pour
  joined again. nrfmicro's passes end after 20 s for 54 with what they had, and one chain of 54 is kept.
- *The rows planned on two layers*: nrfmicro 4, 8 and 8 to 3, 5 and 3 (and back from its call after 10 to 20 s),
  corne-cherry 43, 37 and 39 to 36, 38 and 40; with a budget of work 20 to 11 and 140 to 130. On four layers the
  same option loses, as in D97, by less: buspirate5-rev10 11 to 15, tinytapeout-demo 38 to 43, glasgow-revC3 107
  to 109, cynthion the same. The adapter asks for it on a board of two copper layers and on no other. What
  differs between *0.0.10* and *after* on the boards of four and six layers is what two runs differ by: guide is
  given the same there.
- *lumenpnp-mobo*: 2, 1 and 5 to 4, 8 and 5, all pads that cadlab's fill of the routed board does not reach. Its
  first run counts itself complete, 722 of 722, at 39 to 42 s of the 42 s it has in this hour (21 s in D97's), and
  where it is not back before its deadline the second run, which joins those pads, is not started. With a budget
  of work it has 0, 0 and 1 for 0, 1 and 0.
- *Vias and track*, first order, guide after against the native router: corne-cherry 448 vias and 5816 mm against
  483 and 8276 mm, with 33 more connections open; nrfmicro 49 and 500 mm against 46 and 454 mm; lumenpnp-mobo 704
  against 338; tinytapeout-demo 565 against 240; buspirate5-rev10 449 against 313; sweep-v2.2 42 against 21;
  cynthion 554 against 478; glasgow-revC3 507 against 413; glasgow-revD1 962 against 930. Nothing in guide 0.0.10
  is for vias.
- *What is open, by guide's cause*, of the 412 of the first order: 250 that the budget ended before, 143 pads shut
  in, 12 at a search's node limit, 7 pads the refilled pour does not reach (0.0.9: 237, 152, 24 and 6).
- *Against the native router in this hour*, three orders against its one run: level on the three small boards;
  ahead in every order on lumenpnp-mobo (4, 8 and 5 against 16); within what runs differ by on glasgow-revD1 (242,
  265 and 187 against 244), cynthion (34, 26 and 41 against 33), glasgow-revC3 (52, 36 and 45 against 49) and
  tinytapeout-demo (26, 14 and 6 against 20); behind in every order on nrfmicro (3, 5 and 3 against 0), sweep-v2.2
  (6, 8 and 6 against 3), buspirate5-rev10 (9, 8 and 10 against 2) and corne-cherry (36, 38 and 40 against 3). 412,
  408 and 343 against 370 on the twelve boards, behind in two orders of three; 170, 143 and 156 against 126 on the
  eleven without glasgow-revD1.
- *The generated boards* through the adapter of this change with 0.0.10: one run each with the bench's 120 s, beside 0.0.9 behind D97's adapter in the same hour. Seven of eight complete with no DRC error with both (attiny-2l 0 vias, soic24-2l 159, stm32-4l 51, qfp-qfn-4l 172, fine-4l 105, bga144-4l 248, the same routes); stm32-2l, of two layers and so with its rows planned, is complete with 126 vias and 1300 mm in 5.4 s for 72 vias and 1146 mm in 2.4 s; and big-4l, which the load decides: 59 of its 1275 connections open at 118.8 s, cut, in a first run while a build was going on beside it, and 3 open in 95 s in a second, with the routes of 0.0.9 run beside it to the via (2268 vias, 21 592 mm; 0.0.9 alone 4 open in 96.7 s; complete in 70 s in D97's hour). What guide 0.0.10 does cost in time, which a budget of work does not count: with every net of a board routed, its chains for the pads cut off the pours go on for seconds (lumenpnp-mobo's first run is in "settling what is open" from 25.2 s to 39.0 s with 193 of 193 nets routed, and its cleanup has 3 s for 7).

The default is unchanged (DECISIONS D101). The next table is the one to read against D97's.

**Native router and guide 0.0.11, D101** (guide 0.0.9, 0.0.10 and 0.0.11 from crates.io; the corpus at the
default 60 s time budget; the same machine two hours later, with a load of 4 to 10 from other work, as in D97's
hour; six runs of guide at a time on one thread each, the three rows of guide by turns in one queue; the native
router one run at a time on three threads, seed 1, in the half hour before). Three orders of the nets for guide
(no seed, seeds 2 and 3); the vias, length and time are those of the first.

- *0.0.9*: guide 0.0.9 behind the adapter of D97, master `5a50d3a`.
- *0.0.10*: guide 0.0.10 behind the adapter of this change (`settle` on, the rows planned on two layers).
- *After*: guide 0.0.11 behind the same adapter: 0.0.10 without its chains for the pads cut off their pours.

| board | layers | native: open, vias, length, time | guide 0.0.9: open (three orders); vias, length, time | guide 0.0.10: open | guide after: open (three orders); vias, length, time |
|---|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 4, 460.2 mm, 0.4 s | 0, 0, 0; 7, 496.9 mm, 0.8 s | 0, 0, 0 | 0, 0, 0; 7, 496.9 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 10, 280.9 mm, 0.5 s | 0, 0, 0; 6, 356.2 mm, 0.7 s | 0, 0, 0 | 0, 0, 0; 6, 356.2 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 2, 180.4 mm, 0.4 s | 0, 0, 0; 1, 209.6 mm, 0.3 s | 0, 0, 0 | 0, 0, 0; 1, 209.6 mm, 0.2 s |
| nrfmicro | 2 | 0, 46, 453.6 mm, 11.7 s | 4, 7, 9; 62, 499.4 mm, 27.1 s | 3, 5, 3 | 3, 5, 3; 49, 500.3 mm, 5.9 s |
| sweep-v2.2 | 2 | 3, 21, 2160.9 mm, 53.3 s | 6, 7, 6; 42, 2231.0 mm, 56.0 s cut | 6, 7, 6 | 6, 7, 6; 42, 2231.0 mm, 56.0 s cut |
| corne-cherry | 2 | 3, 483, 8275.6 mm, 29.5 s | 36, 35, 38; 825, 8435.7 mm, 46.8 s cut | 35, 39, 36 | 31, 37, 36; 472, 6084.1 mm, 52.7 s cut |
| tinytapeout-demo | 4 | 6, 274, 4913.3 mm, 59.8 s | 9, 7, 5; 605, 5799.4 mm, 48.8 s cut | 9, 6, 5 | 9, 6, 5; 605, 5799.4 mm, 50.8 s cut |
| buspirate5-rev10 | 4 | 2, 313, 3053.2 mm, 28.3 s | 2, 6, 4; 501, 3815.8 mm, 39.1 s | 0, 6, 4 | 0, 6, 3; 502, 3817.9 mm, 37.9 s |
| lumenpnp-mobo | 4 | 15, 347, 5423.7 mm, 63.6 s cut | 0, 1, 0; 713, 6544.4 mm, 33.2 s | 0, 0, 1 | 0, 1, 0; 713, 6544.4 mm, 32.7 s |
| cynthion | 4 | 27, 478, 3176.5 mm, 61.4 s cut | 19, 14, 23; 657, 4071.6 mm, 57.5 s cut | 17, 14, 23 | 17, 14, 23; 576, 3654.7 mm, 55.0 s cut |
| glasgow-revC3 | 4 | 34, 413, 3426.4 mm, 61.3 s cut | 35, 36, 34; 551, 3936.6 mm, 57.8 s cut | 35, 36, 34 | 35, 37, 34; 551, 3936.6 mm, 57.7 s cut |
| glasgow-revD1 | 6 | 200, 1013, 8861.9 mm, 62.4 s cut | 162, 136, 124; 1085, 7953.6 mm, 57.8 s cut | 162, 136, 124 | 162, 136, 124; 1085, 7953.6 mm, 57.9 s cut |
| all twelve | | 290 | 273, 249, 243 | 267, 249, 236 | 263, 249, 234 |
| the eleven without glasgow-revD1 | | 90 | 111, 113, 119 | 105, 113, 112 | 101, 113, 110 |

Zero DRC errors other than unrouted on every run, no via in a pad of its net, nothing taken out by the
verification, no call of guide over 58.1 s.

What the numbers say:

- *The hour is D97's again*: the native router 290 for 294, guide 0.0.9 273, 249 and 243 for 273, 250 and 238,
  glasgow-revD1 162, 136 and 124 to the connection.
- *From 0.0.9 to after*: 273, 249 and 243 to 263, 249 and 234 on the twelve boards, 111, 113 and 119 to 101, 113
  and 110 on the eleven. nrfmicro 4, 7 and 9 to 3, 5 and 3, and its call is back after 6 to 10 s for 23 to 28; corne-cherry
  36, 35 and 38 to 31, 37 and 36, with 472 vias and 6084 mm for 825 and 8436 mm in the first order;
  buspirate5-rev10 2, 6 and 4 to 0, 6 and 3; cynthion 19, 14 and 23 to 17, 14 and 23, with 576 vias for 657.
  glasgow-revC3 has one more in one order, and no other board more in sum.
- *0.0.11 against 0.0.10*: 267, 249 and 236 to 263, 249 and 234, in corne-cherry's first order and a connection
  here and there. What 0.0.11 is for is a loaded machine: lumenpnp-mobo's call, with every net routed in its first
  run, takes 40.8, 36.7 and 33.4 s with it for 41.1, 45.8 and 37.8 s with 0.0.10 in a run of the two by turns under
  some load, and under the load of the table above its first run was not back in time for its second.
- *Vias and track*, first order, guide after against the native router: lumenpnp-mobo 713 vias against 347,
  tinytapeout-demo 605 against 274, sweep-v2.2 42 against 21, buspirate5-rev10 502 against 313, glasgow-revC3 551
  against 413, cynthion 576 against 478, glasgow-revD1 1085 against 1013, nrfmicro 49 against 46, corne-cherry 472
  against 483 with 28 more connections open. Nothing in these releases is for vias.
- *What is open, by guide's cause*, of the 263 of the first order: 165 that the budget ended before, 92 pads shut
  in, 6 at a search's node limit.
- *Against the native router*, three orders against its one run: level on the three small boards; ahead in every
  order on glasgow-revD1 (162, 136 and 124 against 200), lumenpnp-mobo (0, 1 and 0 against 15) and cynthion (17, 14
  and 23 against 27); within what runs differ by on tinytapeout-demo (9, 6 and 5 against 6), buspirate5-rev10 (0, 6
  and 3 against 2) and glasgow-revC3 (35, 37 and 34 against 34); behind in every order on nrfmicro (3, 5 and 3
  against 0), sweep-v2.2 (6, 7 and 6 against 3) and corne-cherry (31, 37 and 36 against 3). 263, 249 and 234 against
  290 on the twelve boards, ahead in every order; 101, 113 and 110 against 90 on the eleven without glasgow-revD1,
  behind by 11, 23 and 20 where D97 had 18, 21 and 21, and corne-cherry alone is behind by 28, 34 and 33.
- *The generated boards* through the adapter of this change with 0.0.11: one run each with the bench's 120 s, beside 0.0.9 behind D97's adapter at the same time: all eight complete with no DRC error with both, and seven with the same routes (attiny-2l 0 vias in 0.02 s, soic24-2l 159 in 1.1 s, stm32-4l 51 in 1.1 s, qfp-qfn-4l 172 in 4.7 s, fine-4l 105 in 3.5 s, bga144-4l 248 in 15.7 s, big-4l 1994 vias and 21 789 mm in 77 s for 79 s). stm32-2l, of two layers and so with its rows planned, has 126 vias and 1300 mm in 3.2 s for 72 vias and 1146 mm in 1.5 s: what the rows cost a board that completes without them.

The default is unchanged (DECISIONS D101).

**Native router and guide 0.0.12, D104** (guide 0.0.11 and 0.0.12 from crates.io; the corpus at the default 60 s
time budget; the same machine the same evening, with a load of 5 to 9 from other work; six runs of guide at a time
on one thread each, the two rows of guide by turns in one queue; the native router one run at a time on three
threads, seed 1, in the quarter of an hour after). Three orders of the nets for guide (no seed, seeds 2 and 3); the
vias, length and time are those of the first.

- *0.0.11*: guide 0.0.11 behind the adapter of D101 (the rows planned on two layers), master `1a70ba8`.
- *After*: guide 0.0.12 behind the adapter of this change: `negotiate` on, the rows planned on no board.

| board | layers | native: open, vias, length, time | guide 0.0.11: open (three orders); vias, length, time | guide after: open (three orders); vias, length, time |
|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 4, 460.2 mm, 0.5 s | 0, 0, 0; 7, 496.9 mm, 0.8 s | 0, 0, 0; 7, 496.9 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 10, 280.9 mm, 0.5 s | 0, 0, 0; 6, 356.2 mm, 0.7 s | 0, 0, 0; 6, 356.2 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 2, 180.4 mm, 0.5 s | 0, 0, 0; 1, 209.6 mm, 0.3 s | 0, 0, 0; 1, 209.6 mm, 0.3 s |
| nrfmicro | 2 | 0, 46, 453.6 mm, 12.0 s | 3, 5, 3; 49, 500.3 mm, 6.1 s | 2, 4, 3; 35, 521.3 mm, 11.2 s |
| sweep-v2.2 | 2 | 3, 21, 2160.9 mm, 53.7 s | 6, 7, 6; 42, 2231.0 mm, 56.1 s cut | 6, 7, 6; 42, 2231.0 mm, 56.1 s cut |
| corne-cherry | 2 | 3, 483, 8275.6 mm, 31.0 s | 31, 36, 33; 472, 6084.1 mm, 51.9 s cut | 29, 28, 26; 487, 6225.6 mm, 50.4 s cut |
| tinytapeout-demo | 4 | 6, 274, 4913.3 mm, 60.6 s | 13, 7, 5; 569, 5782.3 mm, 58.4 s cut | 9, 7, 5; 605, 5799.4 mm, 50.4 s cut |
| buspirate5-rev10 | 4 | 2, 313, 3053.2 mm, 29.3 s | 0, 6, 3; 502, 3817.9 mm, 35.7 s | 0, 2, 0; 477, 3762.2 mm, 40.2 s |
| lumenpnp-mobo | 4 | 15, 347, 5423.7 mm, 62.7 s cut | 0, 1, 0; 713, 6544.4 mm, 30.8 s | 0, 1, 0; 713, 6544.4 mm, 31.4 s |
| cynthion | 4 | 28, 476, 3177.0 mm, 61.4 s cut | 17, 14, 17; 571, 3666.8 mm, 54.7 s cut | 13, 15, 20; 513, 3537.5 mm, 53.8 s cut |
| glasgow-revC3 | 4 | 36, 415, 3436.4 mm, 60.8 s cut | 35, 35, 34; 538, 3918.3 mm, 57.8 s cut | 35, 36, 34; 538, 3918.3 mm, 57.9 s cut |
| glasgow-revD1 | 6 | 122, 1080, 9069.1 mm, 61.8 s cut | 162, 136, 123; 1085, 7953.6 mm, 57.9 s cut | 162, 135, 123; 1085, 7953.6 mm, 58.0 s cut |
| all twelve | | 215 | 267, 247, 224 | 256, 235, 217 |
| the eleven without glasgow-revD1 | | 93 | 105, 111, 101 | 94, 100, 94 |

Zero DRC errors other than unrouted on every run, no via in a pad of its net, nothing taken out by the
verification, no call of guide over 58.4 s.

What the numbers say:

- *From 0.0.11 to after*: 267, 247 and 224 to 256, 235 and 217 on the twelve boards, 105, 111 and 101 to 94, 100
  and 94 on the eleven. corne-cherry 31, 36 and 33 to 29, 28 and 26; buspirate5-rev10 0, 6 and 3 to 0, 2 and 0;
  nrfmicro 3, 5 and 3 to 2, 4 and 3, with 35 vias for 49; tinytapeout-demo 13, 7 and 5 to 9, 7 and 5; cynthion 17,
  14 and 17 to 13, 15 and 20, the same in sum with 513 vias for 571; glasgow-revC3 one more in one order;
  glasgow-revD1 and sweep-v2.2 the same runs, their minute ending in the first pass.
- *What a first run of corne-cherry does with it*: its first pass has 390 of 432 connections after 7.6 million
  nodes; in the 30 million left of its 42 s it tries 19 to 35 negotiations, keeps 7 to 15, and has 408 to 417
  connections when it is cut. Over
  six orders (seeds 2 to 6 beside none) corne-cherry has 29, 28, 26, 24, 23 and 25 open. With four times the
  budget its first run ends on its own with 415 or 416: what is left is not for want of time.
- *The rows planned*, which D101 turned on for boards of two layers, make no more with the negotiations: over six
  orders corne-cherry 155 with them and 160 without, nrfmicro 18 and 19. They are off on every board again.
- *The native router is not the same from one run to the next on the boards that its budget cuts*: glasgow-revD1
  had 200 open in D101's hour and 115, 130, 192 and 122 in four runs of this evening, lumenpnp-mobo 15 then, 9
  and 15 now; guide's runs of glasgow-revD1 are the same to the connection (162, 135 or 136, 123). So D101's
  290 for the twelve boards is 200 to 215 here, and the sum over twelve boards says which run of
  glasgow-revD1 the native router had.
- *Against the native router*, three orders against its one run of the table: level on the three small boards;
  ahead in every order on lumenpnp-mobo (0, 1 and 0 against 15) and cynthion (13, 15 and 20 against 28); within
  what runs differ by on buspirate5-rev10 (0, 2 and 0 against 2), tinytapeout-demo (9, 7 and 5 against 6) and
  glasgow-revC3 (35, 36 and 34 against 36); behind in every order on nrfmicro (2, 4 and 3 against 0),
  sweep-v2.2 (6, 7 and 6 against 3), corne-cherry (29, 28 and 26 against 3) and, against this run of it,
  glasgow-revD1 (162, 135 and 123 against 122, where the native router's five runs are 115 to 200). 94, 100 and 94 against 93 on the eleven without glasgow-revD1, behind by 1, 7 and 1 where D101 had 11,
  23 and 20, and corne-cherry alone is behind by 26, 25 and 23.
- *Vias and track*, first order, guide after against the native router: lumenpnp-mobo 713 vias against 347,
  tinytapeout-demo 605 against 274, sweep-v2.2 42 against 21, buspirate5-rev10 477 against 313, glasgow-revC3
  538 against 415, cynthion 513 against 476, glasgow-revD1 1085 against 1080, nrfmicro 35 against 46,
  corne-cherry 487 against 483 with 26 more connections open. Nothing in this release is for vias but the rows
  left off.
- *At a budget of work*, 40 million of guide's nodes, one run of guide, three orders (0.0.11 with the rows on
  two layers → 0.0.12's source without): the nine boards that a budget cuts have 831 → 814 open: corne-cherry 127 →
  118, cynthion 63 → 55, buspirate5-rev10 11 → 9, nrfmicro 11 → 9, tinytapeout-demo 38 → 40, glasgow-revC3 107 →
  109, sweep-v2.2 19, lumenpnp-mobo 1 and glasgow-revD1 454 the same.
- *The generated boards* through the adapter of this change with 0.0.12: one run each with the bench's 120 s,
  beside 0.0.11 behind D101's adapter at the same time: all eight complete with no DRC error with both, and seven
  with the same routes (attiny-2l 0 vias in 0.02 s, soic24-2l 159 in 1.2 s, stm32-4l 51 in 1.1 s, qfp-qfn-4l 172
  in 4.3 s, fine-4l 105 in 3.0 s, bga144-4l 248 in 13.9 s, big-4l 1994 vias and 21 789 mm in 67 s for 66 s).
  stm32-2l, whose rows are no longer planned, has 72 vias and 1146 mm in 1.3 s for 126 vias and 1300 mm in 2.9 s.

The default is unchanged (DECISIONS D104).

**Native router and guide 0.0.13, D106** (guide 0.0.12 and 0.0.13 from crates.io behind the same adapter; the
corpus at the default 60 s time budget; the same machine within one hour, with a load of 6 to 15 from other work).
Every figure of "level", "ahead" or "behind" from here on stands on ranges, since the native router is not the
same from one run to the next on the boards that its budget cuts: *four runs* of the native router (seeds 1 to 4,
three threads each, two runs at a time) and *four orders* of the nets for guide (no seed, seeds 2, 3 and 4, one
thread each, four to six runs at a time). Open connections run by run, then the least, the median and the most;
the vias, length and time are those of the first run.

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.12: open by run (min / median / max); vias, length, time | guide 0.0.13: open by run (min / median / max); vias, length, time |
|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 7, 496.9 mm, 0.9 s | 0, 0, 0, 0 (0 / 0 / 0); 7, 496.9 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 356.2 mm, 0.8 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 356.2 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 2, 180.4 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 209.6 mm, 0.3 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 209.6 mm, 0.3 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 12.5 s | 2, 4, 3, 3 (2 / 3 / 4); 35, 521.3 mm, 11.0 s | 5, 2, 3, 3 (2 / 3 / 5); 44, 494.3 mm, 11.7 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 55.5 s | 6, 7, 6, 7 (6 / 6.5 / 7); 42, 2231.0 mm, 55.9 s cut | 3, 3, 3, 3 (3 / 3 / 3); 48, 2290.4 mm, 57.7 s cut |
| corne-cherry | 2 | 3, 4, 0, 3 (0 / 3 / 4); 483, 8275.6 mm, 35.1 s | 34, 28, 26, 27 (26 / 27.5 / 34); 454, 6080.5 mm, 50.0 s cut | 33, 27, 21, 26 (21 / 26.5 / 33); 529, 6656.0 mm, 51.0 s cut |
| tinytapeout-demo | 4 | 8, 5, 6, 3 (3 / 5.5 / 8); 273, 4910.1 mm, 61.2 s | 9, 6, 5, 18 (5 / 7.5 / 18); 605, 5799.4 mm, 51.4 s cut | 9, 7, 5, 16 (5 / 8 / 16); 605, 5799.4 mm, 51.1 s cut |
| buspirate5-rev10 | 4 | 2, 3, 5, 0 (0 / 2.5 / 5); 313, 3053.2 mm, 37.4 s | 1, 7, 7, 4 (1 / 5.5 / 7); 536, 3779.1 mm, 48.8 s cut | 0, 5, 4, 0 (0 / 2 / 5); 484, 3792.0 mm, 36.9 s |
| lumenpnp-mobo | 4 | 15, 8, 29, 12 (8 / 13.5 / 29); 347, 5423.7 mm, 62.2 s cut | 0, 1, 0, 1 (0 / 0.5 / 1); 713, 6544.4 mm, 34.4 s | 0, 1, 0, 1 (0 / 0.5 / 1); 712, 6538.5 mm, 31.5 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3177.0 mm, 61.5 s cut | 17, 15, 25, 22 (15 / 19.5 / 25); 521, 3545.3 mm, 55.7 s cut | 14, 15, 23, 15 (14 / 15 / 23); 512, 3514.5 mm, 54.2 s cut |
| glasgow-revC3 | 4 | 36, 31, 35, 31 (31 / 33 / 36); 415, 3436.4 mm, 60.3 s cut | 36, 36, 41, 34 (34 / 36 / 41); 551, 3930.5 mm, 56.9 s cut | 33, 31, 35, 34 (31 / 33.5 / 35); 560, 4014.4 mm, 57.8 s cut |
| glasgow-revD1 | 6 | 192, 154, 118, 125 (118 / 139.5 / 192); 1046, 8967.0 mm, 62.5 s cut | 162, 137, 124, 121 (121 / 130.5 / 162); 1085, 7953.6 mm, 57.9 s cut | 162, 136, 124, 121 (121 / 130 / 162); 1085, 7953.6 mm, 58.1 s cut |
| all twelve | | 287, 237, 225, 205 (205 / 231 / 287) | 267, 241, 237, 237 (237 / 239 / 267) | 259, 227, 218, 219 (218 / 223 / 259) |
| the eleven without glasgow-revD1 | | 95, 83, 107, 80 (80 / 89 / 107) | 105, 104, 113, 116 (104 / 109 / 116) | 97, 91, 94, 98 (91 / 95.5 / 98) |

Zero DRC errors other than unrouted on every run, no via in a pad of its net, nothing taken out by the
verification; no call of guide over 58.2 s.

What the numbers say:

- *From 0.0.12 to 0.0.13*: the twelve boards have 218 to 259 open over the four orders (median 223) for 237 to
  267 (239), the eleven without glasgow-revD1 91 to 98 (95.5) for 104 to 116 (109). sweep-v2.2 has 3 in every
  order for 6 or 7: the three that the native router has too, and that the board has however it is routed.
  cynthion 14 to 23 for 15 to 25, glasgow-revC3 31 to 35 for 34 to 41, buspirate5-rev10 0 to 5 for 1 to 7,
  corne-cherry 21 to 33 for 26 to 34; nrfmicro 2 to 5 for 2 to 4 and tinytapeout-demo 5 to 16 for 5 to 18, the
  same within what their runs differ by; lumenpnp-mobo and glasgow-revD1 the same runs.
- *Against the native router, by the ranges*: level on four (the three small boards and sweep-v2.2); ahead in
  every run on two (lumenpnp-mobo 0 to 1 against 8 to 29, cynthion 14 to 23 against 28 to 29); ranges that
  overlap on four (buspirate5-rev10 0 to 5 against 0 to 5, glasgow-revC3 31 to 35 against 31 to 36,
  glasgow-revD1 121 to 162 against 118 to 192, tinytapeout-demo 5 to 16 against 3 to 8, where the medians are 8
  and 5.5); behind in every run on two (nrfmicro 2 to 5 against 0, corne-cherry 21 to 33 against 0 to 4). The
  eleven without glasgow-revD1: 91 to 98 against 80 to 107, medians 95.5 and 89.
- *The native router's spread*: glasgow-revD1 118 to 192 and lumenpnp-mobo 8 to 29 over its four seeds; its twelve
  boards are 205 to 287, a wider range than guide's four orders have.
- *Vias and track*, first run, guide 0.0.13 against the native router: lumenpnp-mobo 712 vias against 347,
  tinytapeout-demo 605 against 273, buspirate5-rev10 484 against 313, glasgow-revC3 560 against 415, sweep-v2.2
  48 against 21, corne-cherry 529 against 483, cynthion 512 against 476, glasgow-revD1 1085 against 1046,
  nrfmicro 44 against 46. Nothing in this release is for vias.
- *At a budget of work*, 40 million of guide's nodes, one run of guide, three orders (0.0.12 → 0.0.13, the
  released crates; the same on any machine): the nine boards that a budget cuts have 814 → 801 open: corne-cherry
  118 → 108, glasgow-revC3 109 → 104, sweep-v2.2 19 → 16 (its first pass is cut by that budget), buspirate5-rev10
  9 → 12, nrfmicro 9 → 10, cynthion 55 → 56, tinytapeout-demo 40, lumenpnp-mobo 1 and glasgow-revD1 454 the same.

The default is unchanged (DECISIONS D106).

**Native router and guide 0.0.21, D117** (guide 0.0.21 from crates.io behind the adapter that asks no fill at a run's
end and ends the runs that move routes after two that say the board has no room; cadlab's fill with the reliefs
of its pads kept from one fill to the next, which the native router has as well; the corpus at the default 60 s;
four runs of the native router, seeds 1 to 4, three threads, and four orders of guide, three threads for its
negotiation and one for the rest, two runs of each at a time, side by side in the same quarter of an hour):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.21: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 491.9 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 358.5 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.3 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 199.3 mm, 0.7 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 11.4 s | 0, 0, 0, 0 (0 / 0 / 0); 47, 526.0 mm, 19.2 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 52.0 s | 3, 3, 3, 3 (3 / 3 / 3); 18, 2207.0 mm, 57.7 s |
| corne-cherry | 2 | 3, 4, 0, 3 (0 / 3 / 4); 484, 8273.0 mm, 29.4 s | 1, 0, 2, 1 (0 / 1 / 2); 495, 7412.5 mm, 58.6 s |
| tinytapeout-demo | 4 | 12, 3, 14, 2 (2 / 7.5 / 14); 243, 4938.7 mm, 60.1 s | 0, 6, 0, 1 (0 / 0.5 / 6); 372, 5823.5 mm, 57.7 s cut |
| buspirate5-rev10 | 4 | 4, 2, 12, 5 (2 / 4.5 / 12); 314, 3044.6 mm, 24.6 s | 0, 0, 0, 0 (0 / 0 / 0); 296, 3239.9 mm, 23.1 s |
| lumenpnp-mobo | 4 | 13, 7, 1, 24 (1 / 10 / 24); 340, 5370.3 mm, 61.6 s cut | 0, 0, 0, 0 (0 / 0 / 0); 329, 6064.2 mm, 50.7 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 62.0 s cut | 8, 8, 8, 8 (8 / 8 / 8); 424, 3484.3 mm, 58.4 s |
| glasgow-revC3 | 4 | 36, 31, 33, 31 (31 / 32 / 36); 415, 3436.4 mm, 61.7 s cut | 7, 4, 5, 7 (4 / 6 / 7); 453, 4464.6 mm, 59.4 s cut |
| glasgow-revD1 | 6 | 169, 99, 107, 116 (99 / 111.5 / 169); 1046, 8967.0 mm, 60.8 s cut | 112, 88, 84, 120 (84 / 100 / 120); 1026, 9268.5 mm, 55.4 s cut |
| all twelve | | 268, 178, 199, 212 (178 / 205.5 / 268) | 131, 109, 102, 140 (102 / 120 / 140) |
| the eleven without glasgow-revD1 | | 99, 79, 92, 96 (79 / 94 / 99) | 19, 21, 18, 20 (18 / 19.5 / 21) |


DRC errors other than unrouted, all runs: 0; longest call of guide 59.4 s, of the native router 63.1 s

Pads in pours after the same runs (`CADLAB_BENCH_THERMALS=1`, `board::thermal::census`), the twelve boards together,
by run:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2350 to 2356 | 47, 7, 11, 2 | 159, 131, 146, 134 | 434 to 444 |
| guide 0.0.20, four orders (D116) | 2371 to 2375 | 2, 3, 0, 2 | 64, 71, 75, 75 | 468 to 473 |
| guide 0.0.21, four orders | 2371 to 2375 | 2, 3, 0, 2 | 70, 76, 75, 73 | 468 to 473 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2 against 1 to 2; corne-cherry 1 to 3 against 2 to 4; tinytapeout-demo 11 to 13 against 16 to 21; buspirate5-rev10 25 to 32 against 38 to 44; lumenpnp-mobo 0 against 2 to 3; cynthion 2 to 5 against 2; glasgow-revD1 24 to 28 against 66 to 90. With no neck: sweep-v2.2 0 against 0 to 2; tinytapeout-demo 0 against 0 to 3; lumenpnp-mobo 0 against 0 to 3; cynthion 0 to 3 against 0; glasgow-revD1 0 against 1 to 39.

Vias, track, and the track outside the pads of its net, by the medians of the four runs, guide against the
native router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 5.5 against 4 | 494 against 461 (+7 %) | 431 against 431 (0 %) |
| buspirate-rs232 | 4 against 10 | 358 against 281 (+28 %) | 291 against 272 (+7 %) |
| lumenpnp-ringlight | 6 against 3 | 198 against 177 (+12 %) | 155 against 159 (-3 %) |
| nrfmicro | 44.5 against 47.5 (-6 %) | 531 against 450 (+18 %) | 448 against 414 (+8 %) |
| sweep-v2.2 | 20.5 against 25.5 (-20 %) | 2202 against 2159 (+2 %) | 2121 against 2122 (0 %) |
| corne-cherry | 502.5 against 464.5 (+8 %) | 7595 against 8212 (-8 %) | 7322 against 8027 (-9 %) |
| tinytapeout-demo | 381.5 against 261 (+46 %) | 5922 against 4928 (+20 %) | 5548 against 4704 (+18 %) |
| buspirate5-rev10 | 312 against 308.5 (+1 %) | 3255 against 3088 (+5 %) | 2940 against 2870 (+2 %) |
| lumenpnp-mobo | 335 against 341.5 (-2 %) | 6070 against 5468 (+11 %) | 5467 against 5180 (+6 %) |
| cynthion | 421 against 478 (-12 %) | 3617 against 3184 (+14 %) | 3185 against 2981 (+7 %) |
| glasgow-revC3 | 405.5 against 416 (-3 %) | 4502 against 3462 (+30 %) | 4015 against 3200 (+25 %) |
| glasgow-revD1 | 1016 against 1084 (-6 %) | 9265 against 9147 (+1 %) | 8222 against 8608 (-4 %) |

What the numbers say:

- *The native router has the same fill*, and is not the one of D116's table on glasgow-revD1: 99 to 169 open for
  238 to 289, where a fill took 3 s of each of its verifications. The other boards are within what its seeds gave
  before.
- *Against the native router, by the ranges of the table*: level on buspirate-flash-sop, buspirate-rs232,
  lumenpnp-ringlight, nrfmicro, sweep-v2.2; ahead in every run on buspirate5-rev10, lumenpnp-mobo, cynthion and
  glasgow-revC3; ranges that overlap on corne-cherry, tinytapeout-demo and glasgow-revD1 (84 to 120 against 99 to
  169); behind in every run on none. The twelve boards: 102 to 140 (median 120) against 178 to 268 (205.5); the
  eleven without glasgow-revD1: 18 to 21 (19.5) against 79 to 99 (94).
- *From 0.0.20 to 0.0.21* (D116's table for the first, another day): open as it was on every board but
  tinytapeout-demo, 0, 7, 0, 0 → 0, 6, 0, 1, and glasgow-revC3, 5 to 7 → 4 to 7. Track by the medians against the
  native router's: cynthion +18 % → +14 %, tinytapeout-demo +23 % → +20 %, lumenpnp-mobo +12 % → +11 %, nrfmicro
  +19 % → +18 %, glasgow-revC3 +29 % → +30 %. Vias: glasgow-revD1 1123.5 → 1016 (its needless vias go, 99 to 106
  in every order), tinytapeout-demo 390.5 → 381.5, the others within 1 %.
- *The steps after routing* are made on more boards and not on all: the run for shorter ways in 28 runs of 48 for
  23 (lumenpnp-mobo in every order, 72 to 127 mm, and cynthion in two, for none), the tracks between pads on
  cynthion in two orders (277 mm) and tinytapeout-demo in two (240 and 243 mm). tinytapeout-demo and the glasgows
  end their first run at its share, 42 s, in every order, and have what the runs after it leave.
- *Budgets of nodes*: 40 million, three orders, the nine boards a budget cuts: 620 open, board by board as 0.0.20.
  150 million, two orders: open as 0.0.20; cynthion 3449 and 3398 mm of track for 3558 and 3471.
- *glasgow-revD1 with more time*, one order, three threads: 23 open at three minutes (20 at U8) and 14 at five
  (all at U8, its array of 256 balls); at one minute 62 of its 105 end there (D117).

The default is unchanged (DECISIONS D117).

**Native router and guide 0.0.29, D125** (guide 0.0.29 from crates.io behind the adapter of D124, unchanged; the
corpus at the default 60 s; four runs of the native router, seeds 1 to 4, three threads, and four orders of
guide, three threads for its negotiation, one run of each at a time, side by side in the same forty minutes,
and four more of each on tinytapeout-demo, seeds 5 to 8; the machine's load 9.8 to 34, other work on its 64
threads, where the measurements before had 2 to 6):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.29: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.6 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 439.7 mm, 1.4 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.7 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 293.8 mm, 1.1 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 168.4 mm, 0.9 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 17.0 s | 0, 0, 0, 0 (0 / 0 / 0); 47, 439.4 mm, 9.8 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 62.2 s | 3, 3, 3, 3 (3 / 3 / 3); 22, 2107.5 mm, 50.7 s |
| corne-cherry | 2 | 3, 3, 2, 4 (2 / 3 / 4); 451, 8100.6 mm, 21.6 s | 4, 0, 1, 1 (0 / 1 / 4); 427, 7230.1 mm, 54.4 s cut |
| tinytapeout-demo | 4 | 19, 2, 6, 5, 18, 2, 4, 9 (2 / 5.5 / 19); 244, 4947.5 mm, 61.1 s | 0, 1, 0, 0, 3, 11, 6, 4 (0 / 2 / 11); 270, 4907.8 mm, 53.5 s |
| buspirate5-rev10 | 4 | 4, 2, 4, 8 (2 / 4 / 8); 296, 3156.9 mm, 23.3 s | 0, 0, 0, 0 (0 / 0 / 0); 316, 2998.3 mm, 20.5 s |
| lumenpnp-mobo | 4 | 17, 21, 21, 8 (8 / 19 / 21); 320, 5507.8 mm, 61.2 s cut | 1, 0, 0, 0 (0 / 0 / 1); 347, 5616.4 mm, 52.3 s |
| cynthion | 4 | 27, 30, 30, 29 (27 / 29.5 / 30); 478, 3176.1 mm, 60.1 s cut | 8, 8, 8, 8 (8 / 8 / 8); 425, 3036.7 mm, 46.4 s |
| glasgow-revC3 | 4 | 34, 38, 44, 48 (34 / 41 / 48); 413, 3426.4 mm, 60.9 s cut | 9, 6, 10, 8 (6 / 8.5 / 10); 427, 3623.4 mm, 59.1 s cut |
| glasgow-revD1 | 6 | 155, 178, 144, 164 (144 / 159.5 / 178); 991, 8572.4 mm, 61.7 s cut | 34, 29, 4, 8 (4 / 18.5 / 34); 1160, 9920.2 mm, 49.8 s cut |
| all twelve | | 262, 277, 254, 269 (254 / 265.5 / 277) | 59, 47, 26, 28 (26 / 37.5 / 59) |
| the eleven without glasgow-revD1 | | 107, 99, 110, 105 (99 / 106 / 110) | 25, 18, 22, 20 (18 / 21 / 25) |

The totals are of the first four runs of each. Pads in pours by the necks that reach them, over the twelve
boards, by those runs:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| native router, seeds 1 to 4 | 2356 to 2362 | 19, 14, 17, 17 | 96, 89, 97, 91 | 474 to 479 |
| guide 0.0.29, four orders | 2367 to 2371 | 0, 0, 0, 1 | 36, 43, 34, 38 | 502 to 511 |

Vias and track, the medians of the runs, guide against the native router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 5 against 4 (+25 %) | 440 against 461 (-4 %) | 429 against 431 (-0 %) |
| buspirate-rs232 | 5 against 10 (-50 %) | 297 against 281 (+6 %) | 286 against 272 (+5 %) |
| lumenpnp-ringlight | 3 against 3 (+0 %) | 167 against 177 (-6 %) | 158 against 159 (-1 %) |
| nrfmicro | 42.5 against 47.5 (-11 %) | 437 against 450 (-3 %) | 418 against 414 (+1 %) |
| sweep-v2.2 | 20 against 25.5 (-22 %) | 2122 against 2159 (-2 %) | 2111 against 2122 (-1 %) |
| corne-cherry | 430 against 459 (-6 %) | 7573 against 8103 (-7 %) | 7464 against 7915 (-6 %) |
| tinytapeout-demo | 275.5 against 267 (+3 %) | 5113 against 4889 (+5 %) | 5038 against 4670 (+8 %) |
| buspirate5-rev10 | 307.5 against 302 (+2 %) | 3016 against 3129 (-4 %) | 2894 against 2911 (-1 %) |
| lumenpnp-mobo | 334.5 against 328.5 (+2 %) | 5420 against 5468 (-1 %) | 5292 against 5181 (+2 %) |
| cynthion | 417 against 478.5 (-13 %) | 3069 against 3173 (-3 %) | 2979 against 2969 (+0 %) |
| glasgow-revC3 | 403.5 against 418.5 (-4 %) | 3640 against 3446 (+6 %) | 3542 against 3195 (+11 %) |
| glasgow-revD1 | 1194 against 1003.5 (+19 %) | 10378 against 8732 (+19 %) | 10054 against 8213 (+22 %) |

- *0 DRC errors other than unrouted* in the 104 runs; no pad solid by copper that is no pour with either router.
- *tinytapeout-demo*, the board the release is for: 275.5 vias against 267 (+3 % for 0.0.27's +10 %), 5113 mm
  against 4889 (+5 % for +10 %) and 5038 mm against 4670 outside the pads (+8 % for +13 %); its time 48 to 56 s
  against 44 to 61 s. Beside 0.0.27 under the same load, eight orders of each: 275.5 vias for 309 and 5059 mm
  for 5318, with 29 connections open over the eight for 27 and 55 pads short of two necks for 52 (D125).
- *One pad of guide's without a neck in one order of the four, and one more in tinytapeout-demo's four other
  orders*, where 0.0.27 had none in D124's measurement: a pin of the ground in a header of tinytapeout-demo with
  tracks of two nets between it and the pins beside it, and a pad of +3V3 at its QFN that 0.0.27 has the same
  way in one order of eight under this load. Not mended (D125, "What got worse"). Over the twelve boards 36, 43,
  34 and 38 pads short of two necks for 25, 33, 23 and 38, against the native router's 89 to 97: tinytapeout-demo
  12 in one order for 1, and glasgow-revD1 20 and 21 in two for 8 and 10, by the code of 0.0.27.
- *The eleven other boards are routed by the code of 0.0.27* (the same connections, vias and track to the tenth
  of a millimetre at a budget of nodes), and what moved on them is this measurement's load: corne-cherry 4, 0, 1
  and 1 open for 1, 0, 1 and 1, glasgow-revD1 34, 29, 4 and 8 for 9, 17, 7 and 12, lumenpnp-mobo 1, 0, 0 and 0
  for none, buspirate5-rev10 in 20 to 48 s for 21 to 35 s. The native router has 254 to 277 open over the
  twelve boards for 180 to 240.
- *Budgets of nodes*, 0.0.27 and 0.0.29: 40 million, three orders, the nine boards: 589 open for 594,
  tinytapeout-demo 3, 7 and 10 for 8, 4 and 13 and the eight others as they had. 150 million, two orders:
  tinytapeout-demo none open with 269 and 331 vias for none with 300 and 298, and the six others the same
  connections open (corne-cherry 8 and 0).
- D125 has the table of equal, better and worse by board and measure (26, 29, 3 and 14 of 72 cells; D124: 29,
  29, 1 and 13), the same by the nets that both routers complete (13, 10 and 13 of 36 for 14, 10 and 12), the
  first round of both routers joined connection by connection, and what was tried and not kept.

The default is unchanged (DECISIONS D125).

**Native router and guide 0.0.31, D127** (the corpus at the default 60 s; four runs of the native router, seeds 1
to 4, three threads, and four orders of guide, three threads for its negotiation, one run of each at a time,
side by side in the same half hour; the machine's load 2.9 to 10.3, noted every two minutes, median 7.3, with
another round's measurements on eight of its threads, where D126's was 0.5 to 4.7: the times are to be read
against the native router's here, not against D126's. Measured with a checkout of the commit that 0.0.31 was
released from behind the adapter of this decision):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.31: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 438.9 mm, 0.7 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 285.7 mm, 0.5 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.3 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 166.9 mm, 0.6 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 12.9 s | 0, 0, 0, 0 (0 / 0 / 0); 48, 431.0 mm, 7.1 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 53.4 s | 3, 3, 3, 3 (3 / 3 / 3); 23, 2104.1 mm, 36.8 s |
| corne-cherry | 2 | 3, 3, 2, 4 (2 / 3 / 4); 451, 8100.6 mm, 20.5 s | 7, 0, 1, 1 (0 / 1 / 7); 432, 7023.2 mm, 54.7 s cut |
| tinytapeout-demo | 4 | 19, 2, 5, 5 (2 / 5 / 19); 244, 4947.5 mm, 60.6 s | 0, 1, 0, 0 (0 / 0 / 1); 274, 4884.5 mm, 54.5 s |
| buspirate5-rev10 | 4 | 4, 2, 4, 8 (2 / 4 / 8); 296, 3156.9 mm, 23.5 s | 0, 0, 0, 0 (0 / 0 / 0); 317, 2963.3 mm, 22.5 s |
| lumenpnp-mobo | 4 | 9, 27, 10, 14 (9 / 12 / 27); 339, 5447.7 mm, 61.7 s | 0, 0, 0, 0 (0 / 0 / 0); 341, 5473.7 mm, 54.7 s |
| cynthion | 4 | 27, 29, 30, 28 (27 / 28.5 / 30); 478, 3176.1 mm, 62.2 s cut | 8, 8, 8, 8 (8 / 8 / 8); 428, 2995.4 mm, 36.2 s |
| glasgow-revC3 | 4 | 34, 32, 40, 43 (32 / 37 / 43); 413, 3426.4 mm, 60.2 s cut | 9, 6, 9, 8 (6 / 8.5 / 9); 427, 3610.9 mm, 59.1 s cut |
| glasgow-revD1 | 6 | 135, 128, 94, 99 (94 / 113.5 / 135); 1067, 9050.8 mm, 60.6 s cut | 16, 16, 7, 12 (7 / 14 / 16); 1181, 10497.2 mm, 55.0 s cut |
| all twelve | | 234, 226, 188, 204 (188 / 215 / 234) | 43, 34, 28, 32 (28 / 33 / 43) |
| the eleven without glasgow-revD1 | | 99, 98, 94, 105 (94 / 98.5 / 105) | 27, 18, 21, 20 (18 / 20.5 / 27) |

Pads in pours by the necks that reach them, over the twelve boards, by those runs:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| native router, seeds 1 to 4 | 2350 to 2361 | 15, 7, 2, 6 | 93, 79, 82, 75 | 475 to 480 |
| guide 0.0.31, four orders | 2366 to 2371 | 0, 0, 0, 0 | 33, 32, 30, 35 | 498 to 509 |

Vias and track, the medians of the runs, guide against the native router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 4 against 4 (+0 %) | 442 against 461 (-4 %) | 431 against 431 (-0 %) |
| buspirate-rs232 | 8 against 10 (-20 %) | 284 against 281 (+1 %) | 275 against 272 (+1 %) |
| lumenpnp-ringlight | 3 against 3 (+0 %) | 165 against 177 (-7 %) | 158 against 159 (-1 %) |
| nrfmicro | 42.5 against 47.5 (-11 %) | 429 against 450 (-5 %) | 413 against 414 (-0 %) |
| sweep-v2.2 | 21 against 25.5 (-18 %) | 2115 against 2159 (-2 %) | 2104 against 2122 (-1 %) |
| corne-cherry | 432 against 459 (-6 %) | 7490 against 8103 (-8 %) | 7421 against 7915 (-6 %) |
| tinytapeout-demo | 284.5 against 272.5 (+4 %) | 5038 against 4913 (+3 %) | 4972 against 4692 (+6 %) |
| buspirate5-rev10 | 306.5 against 302 (+1 %) | 2977 against 3129 (-5 %) | 2880 against 2911 (-1 %) |
| lumenpnp-mobo | 334 against 337 (-1 %) | 5326 against 5446 (-2 %) | 5236 against 5160 (+1 %) |
| cynthion | 417 against 479 (-13 %) | 3032 against 3184 (-5 %) | 2961 against 2980 (-1 %) |
| glasgow-revC3 | 403.5 against 417.5 (-3 %) | 3647 against 3448 (+6 %) | 3559 against 3192 (+11 %) |
| glasgow-revD1 | 1176.5 against 1081 (+9 %) | 10603 against 9117 (+16 %) | 10359 against 8595 (+21 %) |

- *0 DRC errors other than unrouted* in the 96 runs; no pad solid by copper that is no pour with either router.
- *No pad of guide's without a neck*, on any board in any of its 48 runs. Pads short of two necks over the
  twelve boards 33, 32, 30 and 35 against the native router's 93, 79, 82 and 75.
- *buspirate-flash-sop* has the native router's four vias in three orders of four, and six in one (D127: the
  route that cut a pad off its pour goes another way, 2.1 mm longer, and the via that joined the pad is gone).
  *buspirate-rs232* has 8 vias and 284 mm for 5 and 297 mm, beside the native router's 10 and 281 mm: its runs
  end on a pad where they come to it, its tracks at the first copper of the pad, and two of its nets have a
  via and 3 mm less.
- *corne-cherry's first order*, 7 connections open here: run again beside 0.0.30 in the same minutes (load 8.4
  to 11.3), the four orders have 1, 0, 1 and 1 open with 0.0.31 and 1, 0, 1 and 2 with 0.0.30, in 55, 35, 50
  and 53 s for 55, 35, 55 and 55. That order's first run ends at its deadline in the middle of its settling.
- *The times of tinytapeout-demo and buspirate5-rev10* are equal to the native router's by these runs, under
  this load; on D126's quiet machine the native router ended five of eight runs of tinytapeout-demo in 34 to
  43 s. guide's own are 53 to 56 s for 50 to 58, and 22 to 34 s for 23 to 34; of buspirate5-rev10's 25 s, 4.3 s
  were its fills measured again in each of six calls, and are 1.6 s (`Zones::fills_clear_of`).
- *Budgets of nodes*, 0.0.30 and 0.0.31: 40 million, three orders, the nine boards: 583 open for 583, each
  board the connections it had. 150 million, two orders, seven boards: the same connections open (corne-cherry
  8 and 0, tinytapeout-demo none and 1), the track 0.2 to 1.5 % shorter on each, and up to ten vias more in
  single orders (tinytapeout-demo 280 for 270, lumenpnp-mobo 344 for 334), for the via at 2 mm in a run that
  is searched for again.
- D127 has the table of equal, better and worse by board and measure (30, 32, 1 and 9 of 72 cells; D126: 29,
  27, 1 and 15), the same by the nets that both routers complete (13, 14 and 9 of 36 for 13, 11 and 12), where
  the time of a run goes on three boards, what was tried and not kept, and what is not done.

The default is unchanged (DECISIONS D127).

**Native router and guide 0.0.32, D128** (seven boards at 60 s, two runs at a time side by side, three threads
each: guide 0.0.30 from crates.io beside guide 0.0.32's `legal_pours`, the load 4.3 to 10.7, median 6.0; then
the native router, seeds 1 to 4 and 5 to 8 on tinytapeout-demo, beside `legal_pours` with `outer_pours`, the
load 5.5 to 11.6, median 7.4. Both options are off in the adapter: with them off the release routes as the
commit before it does. Medians of the vias and of the track in mm; connections open by run):

| board | native router: open; vias; track | guide 0.0.30 | `legal_pours` | `legal_pours` and `outer_pours` |
|---|---|---|---|---|
| corne-cherry | 1/3/2/4; 459; 8103 | 7/0/1/1; 434; 7565 | 5/0/1/2; 429; 7565 | 0/0/0/0; 430; 7540 |
| tinytapeout-demo | 19/1/4/5/1/2/2/9; 273; 4894 | 0/1/0/0/0/0/0/4; 285.5; 5054 | 0/0/0/1/1/0/0/0; 293; 5179 | 0/0/0/0/0/0/0/0; 296.5; 5221 |
| buspirate5-rev10 | 4/2/4/4; 302; 3129 | 0/0/0/0; 308.5; 3018 | 0/0/0/0; 309; 3027 | 0/0/0/0; 305; 3074 |
| glasgow-revC3 | 33/30/39/31; 417.5; 3467 | 9/6/9/8; 403.5; 3663 | 7/8/8/6; 411.5; 3756 | 7/10/8/8; 419; 3840 |
| glasgow-revD1 | 146/137/145/144; 1024; 8906 | 17/14/12/12; 1169; 10756 | 17/16/7/10; 1182; 10747 | 42/49/71/54; 1131; 10187 |
| lumenpnp-mobo | 9/24/10/12; 337; 5446 | 0/0/0/0; 335; 5407 | 1/0/0/0; 335; 5407 | 0/6/0/0; 340; 5494 |
| cynthion | 28/29/30/28; 478; 3184 | 8/8/8/8; 417; 3069 | 8/8/8/8; 410.5; 3055 | 10/8/10/8; 422.5; 3146 |

- Pads with no spoke: none for guide 0.0.30 and for `legal_pours` on the seven boards; with `outer_pours`
  cynthion 2 in one order and glasgow-revD1 1 in each of three. 0 DRC errors other than unrouted in the 128
  runs.
- D128 has the track outside the pads, the pads short of two necks and the times, the stages of
  tinytapeout-demo and buspirate5-rev10 beside the native router's, the budgets of nodes, what was tried and
  not kept, and what remains open.

The default is unchanged (DECISIONS D128).

**Native router and guide 0.0.30, D126** (the corpus at the default 60 s; four runs of the native router, seeds 1
to 4, three threads, and four orders of guide, three threads for its negotiation, one run of each at a time,
side by side in the same half hour, and four more of each on tinytapeout-demo, seeds 5 to 8; the machine's load
0.5 to 4.7, noted every two minutes, median 3.1, where D125's was 9.8 to 34. Measured with a checkout of the
commit that 0.0.30 was released from behind the adapter of this decision; 0.0.30 from crates.io routes five
boards to the same connections and vias at 40 million nodes):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.30: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 439.7 mm, 1.1 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 293.8 mm, 0.9 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.3 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 168.4 mm, 0.7 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 12.1 s | 0, 0, 0, 0 (0 / 0 / 0); 47, 439.4 mm, 6.5 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 51.8 s | 3, 3, 3, 3 (3 / 3 / 3); 22, 2107.5 mm, 39.8 s |
| corne-cherry | 2 | 3, 3, 2, 4 (2 / 3 / 4); 451, 8100.6 mm, 19.4 s | 1, 0, 1, 1 (0 / 1 / 1); 421, 7511.1 mm, 54.5 s cut |
| tinytapeout-demo | 4 | 19, 2, 5, 5, 2, 2, 3, 9 (2 / 4 / 19); 244, 4947.5 mm, 61.1 s | 0, 1, 0, 0, 0, 0, 2, 4 (0 / 0 / 4); 270, 4914.7 mm, 51.2 s |
| buspirate5-rev10 | 4 | 4, 2, 4, 8 (2 / 4 / 8); 296, 3156.9 mm, 23.6 s | 0, 0, 0, 0 (0 / 0 / 0); 316, 2998.4 mm, 24.7 s |
| lumenpnp-mobo | 4 | 8, 12, 7, 14 (7 / 10 / 14); 342, 5561.4 mm, 63.6 s | 0, 0, 0, 0 (0 / 0 / 0); 339, 5516.5 mm, 57.0 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 61.9 s cut | 8, 8, 8, 8 (8 / 8 / 8); 425, 3036.7 mm, 40.0 s |
| glasgow-revC3 | 4 | 36, 31, 35, 31 (31 / 33 / 36); 415, 3436.4 mm, 61.5 s cut | 9, 6, 9, 7 (6 / 8 / 9); 427, 3623.4 mm, 59.1 s cut |
| glasgow-revD1 | 6 | 139, 117, 94, 102 (94 / 109.5 / 139); 1046, 8967.0 mm, 61.2 s cut | 9, 14, 7, 12 (7 / 10.5 / 14); 1200, 10772.3 mm, 57.1 s cut |
| all twelve | | 240, 199, 179, 195 (179 / 197 / 240) | 30, 32, 28, 31 (28 / 30.5 / 32) |
| the eleven without glasgow-revD1 | | 101, 82, 85, 93 (82 / 89 / 101) | 21, 18, 21, 19 (18 / 20 / 21) |

The totals are of the first four runs of each. Pads in pours by the necks that reach them, over the twelve
boards, by those runs:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| native router, seeds 1 to 4 | 2348 to 2361 | 23, 8, 1, 3 | 100, 75, 80, 76 | 477 to 481 |
| guide 0.0.30, four orders | 2367 to 2371 | 0, 0, 0, 0 | 20, 30, 31, 36 | 499 to 510 |

Vias and track, the medians of the runs, guide against the native router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 5 against 4 (+25 %) | 440 against 461 (-4 %) | 429 against 431 (-0 %) |
| buspirate-rs232 | 5 against 10 (-50 %) | 297 against 281 (+6 %) | 286 against 272 (+5 %) |
| lumenpnp-ringlight | 3 against 3 (+0 %) | 167 against 177 (-6 %) | 158 against 159 (-1 %) |
| nrfmicro | 42.5 against 47.5 (-11 %) | 437 against 450 (-3 %) | 418 against 414 (+1 %) |
| sweep-v2.2 | 20 against 25.5 (-22 %) | 2122 against 2159 (-2 %) | 2111 against 2122 (-1 %) |
| corne-cherry | 427 against 459 (-7 %) | 7567 against 8103 (-7 %) | 7460 against 7915 (-6 %) |
| tinytapeout-demo | 285.5 against 273 (+5 %) | 5054 against 4894 (+3 %) | 4977 against 4672 (+7 %) |
| buspirate5-rev10 | 308.5 against 302 (+2 %) | 3018 against 3129 (-4 %) | 2896 against 2911 (-0 %) |
| lumenpnp-mobo | 335 against 345.5 (-3 %) | 5407 against 5462 (-1 %) | 5278 against 5177 (+2 %) |
| cynthion | 417 against 478 (-13 %) | 3069 against 3190 (-4 %) | 2979 against 2986 (-0 %) |
| glasgow-revC3 | 403.5 against 416.5 (-3 %) | 3688 against 3478 (+6 %) | 3582 against 3216 (+11 %) |
| glasgow-revD1 | 1190.5 against 1080.5 (+10 %) | 10763 against 9048 (+19 %) | 10433 against 8527 (+22 %) |

- *0 DRC errors other than unrouted* in the 104 runs; no pad solid by copper that is no pour with either router.
- *No pad of guide's without a neck*, on any board in any of its 52 runs, and in none of tinytapeout-demo's
  sixteen orders measured before the release (D126): D125 had one in each of two orders of that board. Pads
  short of two necks over the twelve boards 20, 30, 31 and 36 against the native router's 100, 75, 80 and 76.
- *Beside 0.0.29 in the same quarter of an hour* (D126, load 2.4 to 4.1): tinytapeout-demo over the same eight
  orders 0, 1, 0, 0, 0, 0, 0 and 4 connections open with both, 285.5 vias with both, 5070 mm for 5108, 47 pads
  short of two necks for 49, none with no neck for one (J14.5, the fourth order), and nothing taken out by the
  check for 62 and 23 items in two orders; corne-cherry over four orders the same connections, vias and track.
  D125's 275.5 vias for that board were measured at a load of 30 and more, with either release.
- *Budgets of nodes*, 0.0.29 and 0.0.30: 40 million, three orders, the nine boards: 583 open for 589,
  tinytapeout-demo 3, 7 and 4 for 3, 7 and 10 and the eight others as they had. 150 million, two orders: the
  six others the same connections open (corne-cherry 8 and 0); tinytapeout-demo none and 1, for none and none,
  with 270 and 332 vias for 269 and 331: the second order's run is cut by the budget at 150 million, where
  0.0.29's ended at 148.7.
- D126 has the table of equal, better and worse by board and measure (29, 27, 1 and 15 of 72 cells; D125: 26,
  29, 3 and 14 under its load; D124: 29, 29, 1 and 13), the same by the nets that both routers complete (13,
  11 and 12 of 36 for 13, 10 and 13), what was tried and not kept, and what of the direction was not attempted.

The default is unchanged (DECISIONS D126).

**Native router and guide 0.0.27, D124** (guide 0.0.27 from crates.io behind the adapter of D124, which keeps the
time of the step for the spokes from the first run on; the corpus at the default 60 s; four runs of the native
router, seeds 1 to 4, three threads, and four orders of guide, three threads for its negotiation, one run of
each at a time, side by side in the same half hour, and four more of each on tinytapeout-demo, seeds 5 to 8;
the machine's load 2.3 to 5.0):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.27: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 439.7 mm, 1.0 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 293.8 mm, 0.8 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.3 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 168.4 mm, 0.7 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 12.5 s | 0, 0, 0, 0 (0 / 0 / 0); 47, 439.4 mm, 6.5 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 50.9 s | 3, 3, 3, 3 (3 / 3 / 3); 22, 2107.5 mm, 39.8 s |
| corne-cherry | 2 | 3, 3, 2, 4 (2 / 3 / 4); 451, 8100.6 mm, 19.9 s | 1, 0, 1, 1 (0 / 1 / 1); 419, 7505.2 mm, 54.8 s cut |
| tinytapeout-demo | 4 | 19, 2, 5, 5, 2, 2, 3, 9 (2 / 4 / 19); 244, 4947.5 mm, 60.8 s | 0, 1, 0, 0, 0, 0, 0, 7 (0 / 0 / 7); 307, 5278.7 mm, 55.1 s |
| buspirate5-rev10 | 4 | 4, 2, 4, 8 (2 / 4 / 8); 296, 3156.9 mm, 22.4 s | 0, 0, 0, 0 (0 / 0 / 0); 316, 2998.3 mm, 20.7 s |
| lumenpnp-mobo | 4 | 8, 12, 7, 22 (7 / 10 / 22); 342, 5561.4 mm, 62.3 s | 0, 0, 0, 0 (0 / 0 / 0); 349, 5620.7 mm, 50.8 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 60.0 s cut | 8, 8, 8, 8 (8 / 8 / 8); 425, 3036.7 mm, 39.5 s |
| glasgow-revC3 | 4 | 36, 31, 35, 31 (31 / 33 / 36); 415, 3436.4 mm, 61.5 s cut | 9, 6, 9, 7 (6 / 8 / 9); 427, 3623.4 mm, 59.1 s cut |
| glasgow-revD1 | 6 | 139, 145, 95, 103 (95 / 121 / 145); 1046, 8967.0 mm, 61.4 s cut | 9, 17, 7, 12 (7 / 10.5 / 17); 1200, 10772.3 mm, 56.7 s cut |
| all twelve | | 240, 227, 180, 204 (180 / 215.5 / 240) | 30, 35, 28, 31 (28 / 30.5 / 35) |
| the eleven without glasgow-revD1 | | 101, 82, 85, 101 (82 / 93 / 101) | 21, 18, 21, 19 (18 / 20 / 21) |

The totals are of the first four runs of each. Pads in pours by the necks that reach them, over the twelve
boards, by those runs:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| native router, seeds 1 to 4 | 2348 to 2366 | 23, 15, 1, 7 | 100, 89, 80, 78 | 478 to 481 |
| guide 0.0.27, four orders | 2368 to 2371 | 0, 0, 0, 0 | 25, 33, 23, 38 | 504 to 513 |

Vias and track, the medians of the runs, guide against the native router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 5 against 4 (+25 %) | 440 against 461 (-4 %) | 429 against 431 (-0 %) |
| buspirate-rs232 | 5 against 10 (-50 %) | 297 against 281 (+6 %) | 286 against 272 (+5 %) |
| lumenpnp-ringlight | 3 against 3 (+0 %) | 167 against 177 (-6 %) | 158 against 159 (-1 %) |
| nrfmicro | 42.5 against 47.5 (-11 %) | 437 against 450 (-3 %) | 418 against 414 (+1 %) |
| sweep-v2.2 | 20 against 25.5 (-22 %) | 2122 against 2159 (-2 %) | 2111 against 2122 (-1 %) |
| corne-cherry | 426 against 459 (-7 %) | 7573 against 8103 (-7 %) | 7464 against 7915 (-6 %) |
| tinytapeout-demo | 300.5 against 273 (+10 %) | 5376 against 4894 (+10 %) | 5272 against 4672 (+13 %) |
| buspirate5-rev10 | 307.5 against 302 (+2 %) | 3016 against 3129 (-4 %) | 2894 against 2911 (-1 %) |
| lumenpnp-mobo | 336 against 343.5 (-2 %) | 5407 against 5489 (-2 %) | 5278 against 5203 (+1 %) |
| cynthion | 417 against 478 (-13 %) | 3069 against 3190 (-4 %) | 2979 against 2986 (-0 %) |
| glasgow-revC3 | 403.5 against 416.5 (-3 %) | 3688 against 3478 (+6 %) | 3582 against 3216 (+11 %) |
| glasgow-revD1 | 1168 against 1061.5 (+10 %) | 10736 against 8982 (+20 %) | 10409 against 8464 (+23 %) |

- *0 DRC errors other than unrouted* in the 104 runs; no pad solid by copper that is no pour with either router.
- *No pad of guide's without a neck*, on any board in any order (0.0.25: two of cynthion's in two orders of
  four), and cynthion's pads short of two necks 1, 1, 1 and 1 against 1, 1, 1 and 1 where 0.0.25 had 5, 5, 1 and
  1: the step that moves routes out of the rooms of spokes is begun and kept in all four orders. Over the
  twelve boards 25, 33, 23 and 38 short of two for 28, 36, 26 and 36, against the native router's 78 to 100.
- *The time*: cynthion ends in 39 to 44 s for 56 to 59 s, against the native router's 60 to 62 s: its eight
  nets with a pad that no track leaves no longer have a search of the whole board in each pass (D124).
  lumenpnp-mobo ends in 51 to 54 s for 56 to 59 s, against 62 s, and sweep-v2.2 in 38 to 40 s; corne-cherry's
  35 to 55 s against 20 to 33 s is as it was.
- *What is open* is level or better on every board but tinytapeout-demo, whose ranges overlap over its eight
  runs. One or two more than 0.0.25 on glasgow-revD1 (9, 17, 7 and 12 for 8, 15, 7 and 12) and glasgow-revC3 (9,
  6, 9 and 7 for 9, 6, 8 and 5): the time of the step for the spokes is kept from the runs on the filled board,
  and on glasgow-revD1 the step is not kept in any order (D124, "What got worse").
- *The vias*: tinytapeout-demo +10 % for 0.0.25's +13 %, buspirate5-rev10 +2 % (307.5 against 302 for 308.5),
  glasgow-revD1 +10 % for +7 % with guide's as they were and the native router's median 28 lower,
  buspirate-flash-sop 5 against 4 as it was.
- *The track* is within 2 % of the native router's or under it on eight boards; over on glasgow-revD1 (+20 %
  for +18 %, guide's as it was; +10 % on the nets both complete), tinytapeout-demo (+10 %, and +7 % in the same
  measurement an hour before with 0.0.26, for +7 %), glasgow-revC3 (+6 % for +8 %) and buspirate-rs232 (+6 %,
  as it was).
- *Budgets of nodes*, 0.0.25 and 0.0.27: 40 million, three orders, the nine boards: 594 open for 594, each board
  as it had. 0.0.26 had 676 (corne-cherry 152 for 86, tinytapeout-demo 39 for 25), which 0.0.27 is for. 150
  million, two orders: the same connections open on every board but glasgow-revC3 (7 and 5 for 6 and 6);
  cynthion in 55 to 57 s for 97 to 109 s.
- D124 has the table of equal, better and worse by board and measure (29, 29, 1 and 13 of 72 cells; D123: 27,
  29, 1 and 15), the same by the nets that both routers complete (14, 10 and 12 of 36 for 13, 11 and 12), what
  was found on cynthion and the two glasgows, and what was tried and not kept.

The default is unchanged (DECISIONS D124).

**Native router and guide 0.0.25, D123** (guide 0.0.25 from crates.io behind the adapter of D123, which begins no
run that moves routes after a first run that moved none and found no way; the corpus at the default 60 s; four
runs of the native router, seeds 1 to 4, three threads, and four orders of guide, three threads for its
negotiation, one run of each at a time, side by side in the same three quarters of an hour, and four more of
each on tinytapeout-demo, seeds 5 to 8; the machine's load 2.2 to 4.0):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.25: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 439.7 mm, 1.1 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 293.8 mm, 0.8 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.3 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 167.5 mm, 0.9 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 13.0 s | 0, 0, 0, 0 (0 / 0 / 0); 47, 439.4 mm, 6.8 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 51.4 s | 3, 3, 3, 3 (3 / 3 / 3); 22, 2107.5 mm, 42.0 s |
| corne-cherry | 2 | 3, 3, 2, 4 (2 / 3 / 4); 451, 8100.6 mm, 19.8 s | 1, 0, 1, 0 (0 / 0.5 / 1); 421, 7502.8 mm, 58.5 s |
| tinytapeout-demo | 4 | 19, 2, 5, 5, 2, 2, 3, 9 (2 / 4 / 19); 244, 4947.5 mm, 61.0 s | 0, 0, 0, 0, 1, 0, 0, 7 (0 / 0 / 7); 315, 5313.3 mm, 58.6 s |
| buspirate5-rev10 | 4 | 4, 2, 4, 8 (2 / 4 / 8); 296, 3156.9 mm, 23.0 s | 0, 0, 0, 0 (0 / 0 / 0); 316, 2998.3 mm, 21.0 s |
| lumenpnp-mobo | 4 | 8, 12, 7, 15 (7 / 10 / 15); 342, 5561.4 mm, 63.5 s | 0, 0, 0, 0 (0 / 0 / 0); 341, 5591.9 mm, 56.8 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 62.2 s cut | 8, 8, 8, 8 (8 / 8 / 8); 442, 3120.6 mm, 58.9 s cut |
| glasgow-revC3 | 4 | 36, 31, 35, 31 (31 / 33 / 36); 415, 3436.4 mm, 60.2 s cut | 9, 6, 8, 5 (5 / 7 / 9); 427, 3709.6 mm, 59.5 s cut |
| glasgow-revD1 | 6 | 117, 108, 94, 96 (94 / 102 / 117); 1086, 9078.3 mm, 61.2 s cut | 8, 15, 7, 12 (7 / 10 / 15); 1200, 10776.3 mm, 57.4 s cut |
| all twelve | | 218, 190, 179, 190 (179 / 190 / 218) | 29, 32, 27, 28 (27 / 28.5 / 32) |
| the eleven without glasgow-revD1 | | 101, 82, 85, 94 (82 / 89.5 / 101) | 21, 17, 20, 16 (16 / 18.5 / 21) |

The totals are of the first four runs of each. Pads in pours by the necks that reach them, over the twelve
boards, by those runs:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| native router, seeds 1 to 4 | 2348 to 2362 | 15, 7, 1, 2 | 91, 77, 80, 74 | 476 to 483 |
| guide 0.0.25, four orders | 2368 to 2371 | 2, 2, 0, 0 | 28, 36, 26, 36 | 505 to 514 |

Vias and track, the medians of the runs, guide against the native router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 5 against 4 (+25 %) | 440 against 461 (-4 %) | 429 against 431 (-0 %) |
| buspirate-rs232 | 5 against 10 (-50 %) | 297 against 281 (+6 %) | 286 against 272 (+5 %) |
| lumenpnp-ringlight | 3 against 3 (+0 %) | 167 against 177 (-6 %) | 158 against 159 (-1 %) |
| nrfmicro | 42.5 against 47.5 (-11 %) | 437 against 450 (-3 %) | 418 against 414 (+1 %) |
| sweep-v2.2 | 20 against 25.5 (-22 %) | 2122 against 2159 (-2 %) | 2110 against 2122 (-1 %) |
| corne-cherry | 427.5 against 459 (-7 %) | 7552 against 8103 (-7 %) | 7446 against 7915 (-6 %) |
| tinytapeout-demo | 308 against 273 (+13 %) | 5254 against 4894 (+7 %) | 5147 against 4672 (+10 %) |
| buspirate5-rev10 | 308.5 against 302 (+2 %) | 3017 against 3129 (-4 %) | 2894 against 2911 (-1 %) |
| lumenpnp-mobo | 335.5 against 343.5 (-2 %) | 5407 against 5462 (-1 %) | 5278 against 5177 (+2 %) |
| cynthion | 422 against 478 (-12 %) | 3125 against 3190 (-2 %) | 3037 against 2986 (+2 %) |
| glasgow-revC3 | 406 against 416.5 (-3 %) | 3756 against 3478 (+8 %) | 3648 against 3216 (+13 %) |
| glasgow-revD1 | 1168 against 1089.5 (+7 %) | 10748 against 9117 (+18 %) | 10421 against 8592 (+21 %) |

- *0 DRC errors other than unrouted* in the 104 runs; no pad solid by copper that is no pour with either router.
- *What is open* is level or better on every board but tinytapeout-demo, whose ranges overlap over its eight
  runs (0, 0, 0, 0, 1, 0, 0 and 7 against 19, 2, 5, 5, 2, 2, 3 and 9). glasgow-revD1 has 7 to 15 open against 94
  to 117, where 0.0.24 had 22 to 39; the twelve boards 27 to 32 against 179 to 218, for 43 to 58.
- *The vias*: tinytapeout-demo +13 % for 0.0.24's +32 %, buspirate5-rev10 +2 % for +9 % (308.5 against 302),
  glasgow-revD1 +7 % for +11 %, buspirate-flash-sop 5 against 4 for 6; lumenpnp-mobo and glasgow-revC3 under the
  native router's where they were level.
- *The track* is within 2 % of the native router's or under it on eight boards; over on glasgow-revD1 (+18 %,
  where it was +11 %, with a third as many connections open; +10 % for +5 % on the nets both complete, D123
  "What got worse"), glasgow-revC3 (+8 % for +14 %), tinytapeout-demo (+7 % for +16 %) and buspirate-rs232
  (+6 %, as it was). Outside the pads, lumenpnp-mobo and cynthion are within 2 % where they were +4 % and +3 %.
- *The time*: sweep-v2.2 ends in 40 to 42 s for 58 s, under the native router's 51 to 52 s; buspirate5-rev10 in
  21 to 36 s for 25 to 41 s. corne-cherry's 33 to 59 s against 20 to 32 s is as it was; cynthion and
  lumenpnp-mobo take 56 to 59 s where they took 49 to 57 s and 49 to 54 s.
- *cynthion's pads short of two necks*: 5, 5, 1 and 1 against 1, 1, 1 and 1 (0.0.24: 0, 1, 1 and 4), the spoke
  step begun in two orders of four as before; over the twelve boards 28, 36, 26 and 36 for 36, 36, 41 and 44.
- *Budgets of nodes*, 0.0.24 and 0.0.25: 40 million, three orders, the nine boards: 594 open for 597
  (glasgow-revC3 39 for 42, the eight others as many as they had). 150 million, two orders: the same connections
  open on every board, tinytapeout-demo with 300 and 298 vias for 320 and 329.
- D123 has the table of equal, better and worse by board and measure (27, 29, 1 and 15 of 72 cells; D122: 24,
  29, 2 and 17), the same by the nets that both routers complete (13, 11 and 12 of 36 for 11, 10 and 15), where
  the two negotiations part, and what was tried and not kept.

The default is unchanged (DECISIONS D123).

**Native router and guide 0.0.24, D122** (guide 0.0.24 from crates.io behind the adapter of D122, which ends the
runs that move routes where one has said all there is, takes the needless vias out whatever the time and leaves
the spoke step its time; the corpus at the default 60 s; four runs of the native router, seeds 1 to 4, three
threads, and four orders of guide, three threads for its negotiation, one run of each at a time, side by side in
the same three quarters of an hour, the machine's load 2.3 to 6.1):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.24: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 440.3 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 294.6 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.3 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 169.1 mm, 0.7 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 12.1 s | 0, 0, 0, 0 (0 / 0 / 0); 47, 441.8 mm, 6.3 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 51.5 s | 3, 3, 3, 3 (3 / 3 / 3); 21, 2136.8 mm, 57.7 s |
| corne-cherry | 2 | 3, 3, 2, 4 (2 / 3 / 4); 451, 8100.6 mm, 19.2 s | 1, 0, 0, 1 (0 / 0.5 / 1); 424, 7503.0 mm, 58.5 s cut |
| tinytapeout-demo | 4 | 19, 2, 5, 5 (2 / 5 / 19); 244, 4947.5 mm, 60.3 s | 0, 3, 0, 0 (0 / 0 / 3); 343, 5445.2 mm, 58.4 s cut |
| buspirate5-rev10 | 4 | 4, 2, 4, 8 (2 / 4 / 8); 296, 3156.9 mm, 22.2 s | 0, 0, 0, 0 (0 / 0 / 0); 328, 3070.0 mm, 41.0 s |
| lumenpnp-mobo | 4 | 8, 12, 7, 15 (7 / 10 / 15); 342, 5561.4 mm, 62.7 s | 0, 0, 0, 0 (0 / 0 / 0); 334, 5567.2 mm, 50.5 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 61.9 s cut | 8, 8, 8, 8 (8 / 8 / 8); 417, 3125.9 mm, 49.1 s |
| glasgow-revC3 | 4 | 36, 32, 33, 31 (31 / 32.5 / 36); 415, 3436.4 mm, 61.6 s cut | 8, 7, 8, 7 (7 / 7.5 / 8); 410, 3812.0 mm, 59.2 s cut |
| glasgow-revD1 | 6 | 111, 110, 94, 102 (94 / 106 / 111); 1086, 9051.5 mm, 61.0 s cut | 28, 22, 39, 36 (22 / 32 / 39); 1170, 9847.8 mm, 55.5 s cut |
| all twelve | | 212, 193, 177, 196 (177 / 194.5 / 212) | 48, 43, 58, 55 (43 / 51.5 / 58) |
| the eleven without glasgow-revD1 | | 101, 83, 83, 94 (83 / 88.5 / 101) | 20, 21, 19, 19 (19 / 19.5 / 21) |

Pads in pours by the necks that reach them, over the twelve boards, by run:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| native router, seeds 1 to 4 | 2347 to 2363 | 12, 7, 1, 4 | 88, 77, 80, 77 | 477 to 485 |
| guide 0.0.24, four orders | 2366 to 2372 | 0, 0, 0, 3 | 36, 36, 41, 44 | 500 to 512 |

Vias and track, the medians of the four runs, guide against the native router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 6 against 4 (+50 %) | 443 against 461 (-4 %) | 431 against 431 (+0 %) |
| buspirate-rs232 | 5 against 10 (-50 %) | 297 against 281 (+6 %) | 286 against 272 (+5 %) |
| lumenpnp-ringlight | 3 against 3 (+0 %) | 168 against 177 (-5 %) | 159 against 159 (-0 %) |
| nrfmicro | 42.5 against 47.5 (-11 %) | 440 against 450 (-2 %) | 421 against 414 (+2 %) |
| sweep-v2.2 | 21 against 25.5 (-18 %) | 2137 against 2159 (-1 %) | 2125 against 2122 (+0 %) |
| corne-cherry | 430 against 459 (-6 %) | 7541 against 8103 (-7 %) | 7430 against 7915 (-6 %) |
| tinytapeout-demo | 358.5 against 272.5 (+32 %) | 5687 against 4913 (+16 %) | 5579 against 4692 (+19 %) |
| buspirate5-rev10 | 329.5 against 302 (+9 %) | 3078 against 3129 (-2 %) | 2962 against 2911 (+2 %) |
| lumenpnp-mobo | 337.5 against 343.5 (-2 %) | 5532 against 5462 (+1 %) | 5408 against 5177 (+4 %) |
| cynthion | 427.5 against 478 (-11 %) | 3165 against 3184 (-1 %) | 3078 against 2981 (+3 %) |
| glasgow-revC3 | 415 against 419.5 (-1 %) | 3949 against 3462 (+14 %) | 3838 against 3200 (+20 %) |
| glasgow-revD1 | 1200.5 against 1084.5 (+11 %) | 10073 against 9090 (+11 %) | 9754 against 8567 (+14 %) |

- *0 DRC errors other than unrouted* in the 96 runs; no pad solid by copper that is no pour with either router.
- *What is open* is level or better on every board but tinytapeout-demo, whose ranges overlap (0, 3, 0 and 0
  against 19, 2, 5 and 5): measured twice more it had 0, 7, 1 and 0, and 2, 0, 0 and 0 beside guide 0.0.23's 2,
  0, 0 and 0 (D122). glasgow-revD1 has 22 to 39 open against 94 to 111.
- *The vias* of corne-cherry are under the native router's (430 against 459, where 0.0.23 had 504) and those of
  lumenpnp-ringlight level (3 against 3 for 4): a net with a pour pays the native router's price for a via on
  two layers. Over: tinytapeout-demo (+32 %), glasgow-revD1 (+11 %), buspirate5-rev10 (+9 %), buspirate-flash-sop
  (6 against 4).
- *The track* is within 2 % of the native router's or under it on eight boards; over on tinytapeout-demo (+16 %),
  glasgow-revC3 (+14 %), glasgow-revD1 (+11 %) and buspirate-rs232 (+6 %). By the nets that both routers
  complete (D122 has the table): glasgow-revC3 +5 %, glasgow-revD1 +5 %, tinytapeout-demo +18 %, buspirate5-rev10
  +4 %.
- *cynthion* ends in 49 to 57 s (57 to 59 s), with the tracks between pads that a pour joins taken out in every
  order (3165 mm for 3284) and the spoke step begun in two orders of four (0, 1, 1 and 4 pads short of two necks
  for 0, 4, 4 and 4).
- *Budgets of nodes*, 0.0.23 and 0.0.24: 40 million, three orders, the nine boards: 597 open for 591, all of it
  corne-cherry's (86 for 80, with 438 to 441 vias for 477 to 557); the eight others the same to the order. 150
  million, two orders: corne-cherry 8 and 0 for 0 and 2, the others as 0.0.23.
- D122 has the table of equal, better and worse by board and measure (24, 29, 2 and 17 of 72 cells; D121: 21, 28,
  3 and 20), the same by the nets that both routers complete, the two routers stage by stage, and what was tried
  and not kept.

The default is unchanged (DECISIONS D122).

**Native router and guide 0.0.23, D121** (guide 0.0.23 from crates.io behind the adapter of D121, which lays a track
no wider than the pad it meets, for either router, and moves routes out of the rooms of spokes where its runs leave
the time; the corpus at the default 60 s; four runs of the native router, seeds 1 to 4, three threads, and four
orders of guide, three threads for its negotiation, one run of each at a time, side by side in the same half hour,
the machine's load 3 to 5 until the last five minutes, 16 to 19 then; a first measurement the same evening at a load
of 31 to 34 is in D121 and not here):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.23: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 440.6 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 297.7 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.3 s | 0, 0, 0, 0 (0 / 0 / 0); 2, 167.9 mm, 0.8 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 11.8 s | 0, 0, 0, 0 (0 / 0 / 0); 47, 442.4 mm, 6.2 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 51.9 s | 3, 3, 3, 3 (3 / 3 / 3); 21, 2134.3 mm, 57.6 s |
| corne-cherry | 2 | 3, 3, 2, 4 (2 / 3 / 4); 451, 8100.6 mm, 19.2 s | 0, 2, 2, 2 (0 / 2 / 2); 502, 7520.3 mm, 51.8 s cut |
| tinytapeout-demo | 4 | 19, 2, 5, 5 (2 / 5 / 19); 244, 4947.5 mm, 60.5 s | 1, 0, 0, 0 (0 / 0 / 1); 359, 5396.1 mm, 57.2 s cut |
| buspirate5-rev10 | 4 | 4, 2, 4, 8 (2 / 4 / 8); 296, 3156.9 mm, 23.8 s | 0, 0, 0, 0 (0 / 0 / 0); 327, 3075.0 mm, 39.1 s |
| lumenpnp-mobo | 4 | 9, 12, 7, 15 (7 / 10.5 / 15); 339, 5447.7 mm, 60.4 s | 0, 0, 0, 0 (0 / 0 / 0); 334, 5565.9 mm, 50.7 s |
| cynthion | 4 | 28, 29, 30, 28 (28 / 28.5 / 30); 476, 3176.6 mm, 62.4 s cut | 8, 8, 8, 8 (8 / 8 / 8); 419, 3268.4 mm, 57.8 s |
| glasgow-revC3 | 4 | 36, 32, 40, 36 (32 / 36 / 40); 415, 3436.4 mm, 60.1 s cut | 9, 7, 9, 9 (7 / 9 / 9); 409, 3807.1 mm, 58.9 s cut |
| glasgow-revD1 | 6 | 113, 169, 149, 135 (113 / 142 / 169); 1083, 9056.4 mm, 60.5 s cut | 95, 90, 30, 38 (30 / 64 / 95); 1216, 8854.8 mm, 60.5 s cut |
| all twelve | | 215, 252, 240, 234 (215 / 237 / 252) | 116, 110, 52, 60 (52 / 85 / 116) |
| the eleven without glasgow-revD1 | | 102, 83, 91, 99 (83 / 95 / 102) | 21, 20, 22, 22 (20 / 21.5 / 22) |

0 DRC errors other than unrouted in the 96 runs; no pad solid by its tracks in any of them (`fab.solid_pad` by
copper that is no pour: 0). Pads in pours, by run (necks counted by their copper, D119):

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| native router, seeds 1 to 4 | 2348 to 2361 | 15, 17, 6, 4 | 93, 93, 85, 76 | 472 to 481 |
| guide 0.0.23, four orders | 2367 to 2373 | 0, 3, 3, 4 | 41, 47, 37, 44 | 504 to 511 |

Vias, track, and the track outside the pads of its net, by the medians of the four runs, guide against the native
router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 6 against 4 (+50 %) | 443 against 461 (-4 %) | 431 against 431 (+0 %) |
| buspirate-rs232 | 5 against 10 (-50 %) | 300 against 281 (+7 %) | 289 against 272 (+6 %) |
| lumenpnp-ringlight | 4 against 3 (+33 %) | 167 against 177 (-6 %) | 158 against 159 (-1 %) |
| nrfmicro | 41.5 against 47.5 (-13 %) | 441 against 450 (-2 %) | 422 against 414 (+2 %) |
| sweep-v2.2 | 21 against 25.5 (-18 %) | 2135 against 2159 (-1 %) | 2124 against 2122 (+0 %) |
| corne-cherry | 504 against 459 (+10 %) | 7433 against 8103 (-8 %) | 7331 against 7915 (-7 %) |
| tinytapeout-demo | 361.5 against 272.5 (+33 %) | 5419 against 4913 (+10 %) | 5313 against 4692 (+13 %) |
| buspirate5-rev10 | 329 against 302 (+9 %) | 3081 against 3129 (-2 %) | 2967 against 2911 (+2 %) |
| lumenpnp-mobo | 337.5 against 342 (-1 %) | 5533 against 5448 (+2 %) | 5410 against 5163 (+5 %) |
| cynthion | 428.5 against 478 (-10 %) | 3284 against 3184 (+3 %) | 3197 against 2981 (+7 %) |
| glasgow-revC3 | 415 against 418.5 (-1 %) | 3928 against 3449 (+14 %) | 3816 against 3191 (+20 %) |
| glasgow-revD1 | 1202.5 against 1037 (+16 %) | 9376 against 8825 (+6 %) | 9053 against 8305 (+9 %) |

What the numbers say:

- *Against the native router, by the ranges of the table*: level on buspirate-flash-sop, buspirate-rs232,
  lumenpnp-ringlight, nrfmicro and sweep-v2.2; ahead in every run on tinytapeout-demo, buspirate5-rev10,
  lumenpnp-mobo, cynthion, glasgow-revC3 and glasgow-revD1 (30 to 95 against 113 to 169); ranges that overlap on
  corne-cherry (0, 2, 2, 2 against 3, 3, 2, 4); behind in every run on none.
- *nrfmicro* has none open in any order (1, 1, 0, 0 with 0.0.22), in 2 to 22 s against the native router's 2 to 12
  s: its negotiation is begun again on finer grids, as the native router's whole run is.
- *The track* is within 2 % of the native router's or under it on seven boards; over on glasgow-revC3 (+14 %, for
  +19 %), tinytapeout-demo (+10 %), buspirate-rs232 (+7 %), glasgow-revD1 (+6 %) and cynthion (+3 %). *The vias* are
  over on tinytapeout-demo (+33 %), glasgow-revD1 (+16 %), corne-cherry (+10 %) and buspirate5-rev10 (+9 %).
- *Budgets of nodes*, 0.0.22 and 0.0.23: 40 million, three orders, the nine boards: 591 open for 593. 150 million,
  two orders: as 0.0.22 or better, glasgow-revC3 6 and 6 for 8 and 6.
- D121 has the table of equal, better and worse by board and measure, the same by the nets that both routers close,
  what a loaded machine did to the first measurement, and what was tried and not kept.

The default is unchanged (DECISIONS D121).

**Native router and guide 0.0.22, D118** (guide 0.0.22 from crates.io behind the adapter that ends the tracks where
they are inside the pads of their net; cadlab at `921ac7a`, the fill and the census of D119; the corpus at the
default 60 s; four runs of the native router, seeds 1 to 4, three threads, and four orders of guide, three threads
for its negotiation, one run of each at a time, side by side in the same fifty minutes, the machine's load 10 to 19):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.22: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 440.6 mm, 0.7 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 297.7 mm, 0.6 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.3 s | 0, 0, 0, 0 (0 / 0 / 0); 2, 167.9 mm, 0.8 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 12.4 s | 1, 1, 0, 0 (0 / 0.5 / 1); 39, 451.7 mm, 30.9 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 55.5 s | 3, 3, 3, 3 (3 / 3 / 3); 21, 2134.3 mm, 56.7 s cut |
| corne-cherry | 2 | 3, 3, 2, 4 (2 / 3 / 4); 451, 8100.6 mm, 20.2 s | 3, 2, 0, 2 (0 / 2 / 3); 484, 7274.8 mm, 58.5 s cut |
| tinytapeout-demo | 4 | 19, 2, 5, 5 (2 / 5 / 19); 244, 4947.5 mm, 60.2 s | 0, 0, 0, 0 (0 / 0 / 0); 359, 5189.1 mm, 57.9 s |
| buspirate5-rev10 | 4 | 4, 2, 4, 8 (2 / 4 / 8); 296, 3156.9 mm, 22.6 s | 0, 0, 0, 0 (0 / 0 / 0); 317, 3114.9 mm, 19.7 s |
| lumenpnp-mobo | 4 | 8, 11, 7, 22 (7 / 9.5 / 22); 342, 5561.4 mm, 63.7 s | 0, 0, 0, 0 (0 / 0 / 0); 347, 5589.1 mm, 52.9 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 62.0 s cut | 8, 8, 8, 8 (8 / 8 / 8); 423, 3436.7 mm, 58.9 s |
| glasgow-revC3 | 4 | 36, 32, 40, 40 (32 / 38 / 40); 415, 3436.4 mm, 60.2 s cut | 9, 4, 5, 10 (4 / 7 / 10); 415, 3986.0 mm, 59.0 s cut |
| glasgow-revD1 | 6 | 149, 174, 144, 184 (144 / 161.5 / 184); 1029, 8897.0 mm, 60.2 s cut | 39, 150, 77, 65 (39 / 71 / 150); 1240, 9951.8 mm, 58.3 s cut |
| all twelve | | 250, 256, 234, 294 (234 / 253 / 294) | 63, 168, 93, 88 (63 / 90.5 / 168) |
| the eleven without glasgow-revD1 | | 101, 82, 90, 110 (82 / 95.5 / 110) | 24, 18, 16, 23 (16 / 20.5 / 24) |

0 DRC errors other than unrouted in the 96 runs. Pads in pours, by run (necks counted by their copper, D119):

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| native router, seeds 1 to 4 | 2348 to 2365 | 24, 15, 17, 15 | 101, 91, 96, 90 | 472 to 481 |
| guide 0.0.22, four orders | 2373 to 2376 | 2, 3, 2, 4 | 45, 47, 36, 51 | 506 to 522 |

Vias, track, and the track outside the pads of its net, by the medians of the four runs, guide against the
native router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 6 against 4 (+50 %) | 443 against 461 (-4 %) | 431 against 431 (+0 %) |
| buspirate-rs232 | 5 against 10 (-50 %) | 300 against 281 (+7 %) | 289 against 272 (+6 %) |
| lumenpnp-ringlight | 4 against 3 (+33 %) | 167 against 177 (-6 %) | 158 against 159 (-1 %) |
| nrfmicro | 42 against 47.5 (-12 %) | 448 against 450 (-0 %) | 429 against 414 (+4 %) |
| sweep-v2.2 | 21 against 25.5 (-18 %) | 2135 against 2159 (-1 %) | 2124 against 2122 (+0 %) |
| corne-cherry | 498.5 against 459 (+9 %) | 7382 against 8103 (-9 %) | 7278 against 7915 (-8 %) |
| tinytapeout-demo | 328.5 against 272.5 (+21 %) | 5231 against 4913 (+6 %) | 5130 against 4692 (+9 %) |
| buspirate5-rev10 | 319 against 302 (+6 %) | 3126 against 3129 (-0 %) | 3011 against 2911 (+3 %) |
| lumenpnp-mobo | 342.5 against 344 (-0 %) | 5576 against 5533 (+1 %) | 5450 against 5246 (+4 %) |
| cynthion | 420 against 478 (-12 %) | 3261 against 3190 (+2 %) | 3159 against 2986 (+6 %) |
| glasgow-revC3 | 419 against 418.5 (+0 %) | 4099 against 3446 (+19 %) | 3983 against 3194 (+25 %) |
| glasgow-revD1 | 1160.5 against 1004.5 (+16 %) | 9250 against 8813 (+5 %) | 8923 against 8294 (+8 %) |

What the numbers say:

- *Against the native router, by the ranges of the table*: level on buspirate-flash-sop, buspirate-rs232,
  lumenpnp-ringlight, sweep-v2.2; ahead in every run on tinytapeout-demo, buspirate5-rev10, lumenpnp-mobo, cynthion
  and glasgow-revC3; ranges that overlap on nrfmicro (1, 1, 0, 0 against 0), corne-cherry and glasgow-revD1 (39 to
  150 against 144 to 184, medians 71 against 161.5); behind in every run on none.
- *glasgow-revD1* is what the release is for: seven rounds of its negotiation in the first run's share for four,
  its array's vias planned; 2 open at three minutes and at five for 23 and 14. Its first run still ends at its
  share, and what a minute leaves moves with the machine: the order with 150 had 40 and 120 run alone afterwards.
- *The track* is within 2 % of the native router's or under it on eight boards, with the ends of the tracks cut
  where they are inside their pads (`pad_ends`: the same copper, counted as the native router's is); over on
  buspirate-rs232 (+7 %), tinytapeout-demo (+6 %), glasgow-revD1 (+5 %) and glasgow-revC3 (+19 %).
- *Worse than 0.0.21*: nrfmicro's one connection in two orders of four; glasgow-revD1's vias with its connections.
- *Budgets of nodes*, 0.0.21 and 0.0.22 on this fill: 40 million, three orders, the nine boards: 593 open for 615.
  150 million, two orders: as 0.0.21 or within one, glasgow-revC3 8 and 6 for 9 and 9.
- D118 has the table of equal, better and worse by board and measure, freerouting beside the two, and what was
  tried and not kept.

- *freerouting 2.5.0 beside the two* on the CI runner (the `freerouting-corpus` job on this branch, 60 s, one run
  of each): unrouted by cadlab's DRC over the twelve boards, freerouting 1685 without pours and 2898 with the
  pours as planes, the native router 256, guide 146; D118 has the table by board.

The default is unchanged (DECISIONS D118).

**Native router and guide 0.0.20, D116** (guide 0.0.20 from crates.io behind the adapter that takes the tracks
between pads that a pour joins out by the fill and has guide look for shorter ways once its runs are over; the fill
of D112; the corpus at the default 60 s; four runs of the native router, seeds 1 to 4, three threads, and four
orders of guide, three threads for its negotiation and one for the rest, two runs of each at a time, side by side
in the same half hour):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.20: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 491.9 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 358.5 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 199.3 mm, 1.0 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 11.5 s | 0, 0, 0, 0 (0 / 0 / 0); 47, 530.6 mm, 19.5 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 51.3 s | 3, 3, 3, 3 (3 / 3 / 3); 18, 2207.0 mm, 58.3 s |
| corne-cherry | 2 | 3, 4, 0, 3 (0 / 3 / 4); 484, 8273.0 mm, 29.2 s | 1, 0, 2, 1 (0 / 1 / 2); 495, 7398.6 mm, 58.6 s |
| tinytapeout-demo | 4 | 15, 3, 15, 2 (2 / 9 / 15); 238, 4930.2 mm, 61.1 s | 0, 7, 0, 0 (0 / 0 / 7); 327, 5465.8 mm, 57.4 s |
| buspirate5-rev10 | 4 | 4, 2, 12, 5 (2 / 4.5 / 12); 314, 3044.6 mm, 30.0 s | 0, 0, 0, 0 (0 / 0 / 0); 296, 3243.4 mm, 28.7 s |
| lumenpnp-mobo | 4 | 13, 9, 5, 13 (5 / 11 / 13); 340, 5370.3 mm, 62.1 s cut | 0, 0, 0, 0 (0 / 0 / 0); 331, 6141.6 mm, 59.2 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 62.4 s cut | 8, 8, 8, 8 (8 / 8 / 8); 423, 3761.1 mm, 58.7 s |
| glasgow-revC3 | 4 | 36, 37, 35, 31 (31 / 35.5 / 37); 415, 3436.4 mm, 60.4 s cut | 7, 5, 5, 7 (5 / 6 / 7); 453, 4464.6 mm, 59.3 s cut |
| glasgow-revD1 | 6 | 241, 289, 238, 250 (238 / 245.5 / 289); 898, 7744.3 mm, 60.0 s cut | 106, 87, 84, 120 (84 / 96.5 / 120); 1139, 9518.3 mm, 57.9 s cut |
| all twelve | | 343, 376, 337, 335 (335 / 340 / 376) | 125, 110, 102, 139 (102 / 117.5 / 139) |
| the eleven without glasgow-revD1 | | 102, 87, 99, 85 (85 / 93 / 102) | 19, 23, 18, 19 (18 / 19 / 23) |

DRC errors other than unrouted, all runs: 0; longest call of guide 59.5 s, of the native router 62.9 s

Pads in pours after the same runs (`CADLAB_BENCH_THERMALS=1`, `board::thermal::census`), the twelve boards together,
by run:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2348 to 2355 | 36, 49, 38, 38 | 154, 159, 162, 157 | 432 to 441 |
| guide 0.0.19, four orders (D115) | 2372 to 2374 | 2, 3, 0, 0 | 73, 76, 75, 77 | 469 to 473 |
| guide 0.0.20, four orders | 2371 to 2375 | 2, 3, 0, 2 | 64, 71, 75, 75 | 468 to 473 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2 against 1 to 2; corne-cherry 1 to 3 against 2 to 4; tinytapeout-demo 8 to 12 against 16 to 21; buspirate5-rev10 26 to 32 against 38 to 44; lumenpnp-mobo 0 against 2 to 3; cynthion 2 to 4 against 2; glasgow-revD1 23 to 28 against 85 to 96. With no neck: sweep-v2.2 0 against 0 to 2; tinytapeout-demo 0 against 0 to 3; lumenpnp-mobo 0 against 0 to 3; cynthion 0 to 3 against 0; glasgow-revD1 0 against 28 to 47.

Vias, track, and the track outside the pads of its net (`CADLAB_BENCH_VERBOSE=1`; a track of guide's goes to the
middle of its pad, the native router's ends at the first point of its grid on the pad), by the medians of the four
runs, guide against the native router:

| board | vias | track, mm | track outside the pads of its net, mm |
|---|---|---|---|
| buspirate-flash-sop | 5.5 against 4 | 494 against 461 (+7 %) | 431 against 431 (0 %) |
| buspirate-rs232 | 4 against 10 | 358 against 281 (+28 %) | 291 against 272 (+7 %) |
| lumenpnp-ringlight | 6 against 3 | 198 against 177 (+12 %) | 155 against 159 (-3 %) |
| nrfmicro | 44.5 against 47.5 (-6 %) | 536 against 450 (+19 %) | 450 against 414 (+9 %) |
| sweep-v2.2 | 20.5 against 25.5 (-20 %) | 2202 against 2159 (+2 %) | 2121 against 2122 (0 %) |
| corne-cherry | 502.5 against 464.5 (+8 %) | 7595 against 8212 (-8 %) | 7322 against 8027 (-9 %) |
| tinytapeout-demo | 390.5 against 261 (+50 %) | 6035 against 4924 (+23 %) | 5669 against 4700 (+21 %) |
| buspirate5-rev10 | 313 against 308.5 (+1 %) | 3247 against 3088 (+5 %) | 2929 against 2870 (+2 %) |
| lumenpnp-mobo | 336 against 341 (-1 %) | 6167 against 5511 (+12 %) | 5555 against 5220 (+6 %) |
| cynthion | 423 against 478 (-12 %) | 3758 against 3184 (+18 %) | 3263 against 2981 (+9 %) |
| glasgow-revC3 | 403 against 421 (-4 %) | 4486 against 3478 (+29 %) | 3998 against 3216 (+24 %) |
| glasgow-revD1 | 1123.5 against 887.5 (+27 %) | 9392 against 7708 (+22 %) | 8321 against 7212 (+15 %) |

What the numbers say:

- *Against the native router, by the ranges of the table*: level on buspirate-flash-sop, buspirate-rs232,
  lumenpnp-ringlight, nrfmicro, sweep-v2.2; ahead in every run on buspirate5-rev10 0 against 2 to 12, lumenpnp-mobo
  0 against 5 to 13, cynthion 8 against 28 to 29, glasgow-revC3 5 to 7 against 31 to 37 and glasgow-revD1 84 to 120
  against 238 to 289; ranges that overlap on corne-cherry 0 to 2 against 0 to 4 and tinytapeout-demo 0 to 7 against
  2 to 15; behind in every run on none. The twelve boards: 102 to 139 (median 117.5) against 335 to 376 (340); the
  eleven without glasgow-revD1: 18 to 23 (19) against 85 to 102 (93).
- *From 0.0.19 to 0.0.20* (D115's table for the first, another hour): glasgow-revC3 29 to 34 open → 5 to 7, by the
  ways out of its array's second ring and the dog-bones away from its middle; glasgow-revD1 93 to 112 → 84 to 120;
  corne-cherry 2, 1, 0, 0 → 1, 0, 2, 1; tinytapeout-demo 0, 7, 1, 0 → 0, 7, 0, 0; the others as they were; the
  twelve boards 142 to 157 → 102 to 139.
- *Track* is within a tenth of the native router's on four boards (buspirate-flash-sop, sweep-v2.2, corne-cherry,
  buspirate5-rev10) and, counted outside the pads, on five more (buspirate-rs232, lumenpnp-ringlight, nrfmicro,
  lumenpnp-mobo, cynthion). From 0.0.19, by the medians against the native router's: buspirate5-rev10 +17 % → +5 %,
  lumenpnp-mobo +20 % → +12 %, corne-cherry +5 % → -8 %, nrfmicro +22 % → +19 %, cynthion +19 % → +18 %,
  tinytapeout-demo +23 % as it was, glasgow-revD1 +21 % → +22 %, glasgow-revC3 +12 % → +29 % with some 25
  connections more. What went is the track between pads that a pour joins (lumenpnp-mobo 520 to 552 mm in every
  order, corne-cherry 310 to 384 mm, buspirate5-rev10 226 to 250 mm, tinytapeout-demo 201 and 223 mm in two orders)
  and, where the runs end before the minute, a shorter way (buspirate5-rev10 75 to 188 mm, tinytapeout-demo 322 mm
  in one order). The boards that a minute does not finish have little of either: no time is kept for the second
  (D116 has what keeping it cost). At 150 million nodes, where both are made: tinytapeout-demo 5690 and 5432 mm
  with 358 and 341 vias, lumenpnp-mobo 6064 and 6013 mm, cynthion 3558 and 3471 mm.
- *Vias*: corne-cherry 473 to 525 against 445 to 505, 8 % over by the medians for 22 % with 0.0.19; tinytapeout-demo
  327 to 462 against 238 to 290, as wide as it was (362 to 462), 50 % over by the medians for 41 %.
- *Budgets of nodes* (the same on any machine): 40 million, three orders, the nine boards a budget cuts: 620 open
  for 675 with 0.0.19 (glasgow-revC3 41 for 100, glasgow-revD1 437 for 433, the other seven the same). 150
  million, two orders: corne-cherry 1 and 0 for 2 and 1, glasgow-revC3 5 and 9 for 24 and 26, tinytapeout-demo 0
  and 3, cynthion 8 and 8, sweep-v2.2 3 and 3, buspirate5-rev10, lumenpnp-mobo and nrfmicro none, as before.

The default is unchanged (DECISIONS D116).

**Native router and guide 0.0.19, D115** (guide 0.0.19 from crates.io behind the adapter that names the heat pads
to it and takes its needless vias out by the fill of the routed board; the fill of D112; the corpus at the default
60 s; four runs of the native router, seeds 1 to 4, three threads, and four orders of guide, three threads for its
negotiation and one for the rest, two runs of each at a time, side by side in the same half hour):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.19: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 497.1 mm, 0.4 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 358.6 mm, 0.4 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 205.3 mm, 0.8 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 11.3 s | 0, 0, 0, 0 (0 / 0 / 0); 48, 536.2 mm, 19.1 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 51.3 s | 3, 3, 3, 3 (3 / 3 / 3); 18, 2210.5 mm, 58.4 s |
| corne-cherry | 2 | 3, 4, 0, 3 (0 / 3 / 4); 484, 8273.0 mm, 28.9 s | 2, 1, 0, 0 (0 / 0.5 / 2); 604, 8804.6 mm, 58.3 s |
| tinytapeout-demo | 4 | 14, 3, 15, 2 (2 / 8.5 / 15); 240, 4932.7 mm, 61.4 s | 0, 7, 1, 0 (0 / 0.5 / 7); 372, 6066.9 mm, 59.1 s cut |
| buspirate5-rev10 | 4 | 4, 2, 12, 5 (2 / 4.5 / 12); 314, 3044.6 mm, 30.1 s | 0, 0, 0, 0 (0 / 0 / 0); 308, 3569.4 mm, 22.3 s |
| lumenpnp-mobo | 4 | 13, 9, 5, 33 (5 / 11 / 33); 340, 5370.3 mm, 61.8 s cut | 0, 0, 0, 0 (0 / 0 / 0); 331, 6686.0 mm, 54.4 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 62.0 s cut | 8, 8, 8, 8 (8 / 8 / 8); 405, 3762.2 mm, 57.9 s |
| glasgow-revC3 | 4 | 36, 31, 35, 31 (31 / 33 / 36); 415, 3436.4 mm, 61.4 s cut | 29, 30, 33, 34 (29 / 31.5 / 34); 359, 4006.2 mm, 59.4 s cut |
| glasgow-revD1 | 6 | 241, 288, 376, 307 (241 / 297.5 / 376); 898, 7744.3 mm, 64.2 s cut | 105, 93, 112, 105 (93 / 105 / 112); 1112, 9233.4 mm, 58.0 s cut |
| all twelve | | 342, 369, 475, 412 (342 / 390.5 / 475) | 147, 142, 157, 150 (142 / 148.5 / 157) |
| the eleven without glasgow-revD1 | | 101, 81, 99, 105 (81 / 100 / 105) | 42, 49, 45, 45 (42 / 45 / 49) |

DRC errors other than unrouted, all runs: 0; longest call 64.2 s

Pads in pours after the same runs (`CADLAB_BENCH_THERMALS=1`, `board::thermal::census`), the twelve boards together,
by run:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2348 to 2355 | 36, 49, 49, 49 | 154, 159, 170, 164 | 432 to 441 |
| guide 0.0.16, four orders (D110) | 2355 to 2363 | 2, 2, 2, 2 | 91, 82, 89, 100 | 442 to 461 |
| guide 0.0.17, four orders (D113) | 2361 to 2366 | 1, 1, 1, 3 | 97, 91, 88, 97 | 459 to 468 |
| guide 0.0.18, four orders (D114) | 2350 to 2367 | 1, 1, 1, 3 | 100, 94, 90, 95 | 458 to 467 |
| guide 0.0.19, four orders | 2372 to 2374 | 2, 3, 0, 0 | 73, 76, 75, 77 | 469 to 473 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2 against 1 to 2; corne-cherry 0 to 2 against 2 to 4; tinytapeout-demo 9 to 14 against 16 to 21; buspirate5-rev10 29 to 34 against 38 to 44; lumenpnp-mobo 0 against 2 to 3; cynthion 0 to 4 against 2; glasgow-revD1 23 to 28 against 85 to 96. With no neck: sweep-v2.2 0 against 0 to 2; tinytapeout-demo 0 against 0 to 3; lumenpnp-mobo 0 against 0 to 3; cynthion 0 to 3 against 0; glasgow-revD1 0 against 28 to 47.

What the numbers say:

- *Against the native router, by the ranges of the table*: level on buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2; ahead in every run on buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 5 to 33; cynthion 8 against 28 to 29; glasgow-revD1 93 to 112 against 241 to 376;
ranges that overlap on corne-cherry 0 to 2 against 0 to 4; tinytapeout-demo 0 to 7 against 2 to 15; glasgow-revC3 29 to 34 against 31 to 36; behind in every run on none. The twelve boards: 142 to 157 (median 148.5) against 342 to 475 (390.5); the
  eleven without glasgow-revD1: 42 to 49 (45) against 81 to 105 (100).
- *From 0.0.18 to 0.0.19* (D114's table for the first, another hour): cynthion 11 to 12 open → 8, glasgow-revD1
  118 to 135 → 93 to 112, glasgow-revC3 31 to 35 → 29 to 34, corne-cherry 0 to 2 → 0 to 2 (2, 1, 0, 0 both),
  tinytapeout-demo 0, 4, 0, 4 → 0, 7, 1, 0, the others as they were; the twelve boards 164 to 187 → 142 to 157.
- *glasgow-revD1* is negotiated now, at twice the pitch: five rounds in half of its first run, where 0.0.17 and
  0.0.18 gave its negotiation up after a second (D113). *cynthion*'s three fewer are the exposed pads of U8, U9
  and U11, joined by vias in them; the 8 left are the Kelvin terminals of its four sense resistors, which the
  native router leaves too (D115).
- *Vias and track*: vias, the range of the four runs, lumenpnp-mobo 331 to 348 against 309 to 343, tinytapeout-demo 362 to 462 against 240 to 290, corne-cherry 522 to 609 against 445 to 505, buspirate5-rev10 308 to 348 against 291 to 314, cynthion 405 to 430 against 469 to 482, glasgow-revC3 330 to 359 against 415 to 439, glasgow-revD1 1080 to 1114 against 826 to 898, sweep-v2.2 18 to 24 against 21 to 28, nrfmicro 41 to 60 against 46 to 51. By the medians: lumenpnp-mobo 336 against 339.5 (-1 %), tinytapeout-demo 369 against 261 (+41 %), corne-cherry 566.5 against 464.5 (+22 %), buspirate5-rev10 315 against 308.5 (+2 %), cynthion 416 against 478 (-13 %), glasgow-revC3 343.5 against 416.5 (-18 %), glasgow-revD1 1100 against 863 (+27 %). Track, the medians:
  lumenpnp-mobo 6711.5 mm against 5592 (+20 %), tinytapeout-demo 6057 mm against 4925 (+23 %), corne-cherry 8650 mm against 8212 (+5 %), buspirate5-rev10 3625 mm against 3088 (+17 %), cynthion 3785 mm against 3184.5 (+19 %), glasgow-revC3 3894.5 mm against 3478 (+12 %), glasgow-revD1 9132 mm against 7552 (+21 %), sweep-v2.2 2205 mm against 2159 (+2 %), nrfmicro 547 mm against 450 (+22 %). The targets were the vias of lumenpnp-mobo, tinytapeout-demo and corne-cherry within 15 % of the native router's
median, with no more connections open on any board. *lumenpnp-mobo meets it*: 331 to 348 against 309 to 343
(medians 336 against 339.5; 402 to 411 with 0.0.18), its nets with zones at 99 to 103 vias for the native router's
92 to 145 and the others at 232 to 245 for 198 to 217. *tinytapeout-demo does not*: 362 to 462 against 240 to 290
(369 against 261, 41 % over, for 54 % with 0.0.18, 385 to 427), its nets with zones at 66 to 92 for 42 to 67 and
the others at 296 to 370 for 195 to 232; and one order of its four has 7 open where 0.0.18 had 4 at most (0, 7, 1,
0 for 0, 4, 0, 4): its first run left 4, the run on the fixed board ended at 52 s with no fewer, and there was no
time for a run that moves routes. *corne-cherry does not*: 522 to 609 against 445 to 505 (566.5 against 464.5,
22 % over; 534 to 625 with 0.0.18), its two grounds at 134 to 162 for 63 to 75 and its other nets at 375 to 454
for 380 to 440. Beside the targets, against the native router and 0.0.18: buspirate5-rev10 308 to 348 against
291 to 314 (349 to 372), cynthion 405 to 430 against 469 to 482 (604 to 699), glasgow-revC3 330 to 359 against
415 to 439 (379 to 403), glasgow-revD1 1080 to 1114 against 826 to 898 (1202 to 1253).

**Native router and guide 0.0.18, D114** (guide 0.0.18 from crates.io behind the adapter that gives it cadlab's
fill of the zones around its routes when it asks; the fill of D112; the corpus at the default 60 s; four runs of the
native router, seeds 1 to 4, three threads, and four orders of guide, three threads for its negotiation and one for
the rest, two runs of each at a time, side by side in the same half hour):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.18: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 497.1 mm, 0.4 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 358.6 mm, 0.4 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 206.1 mm, 0.7 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 11.4 s | 0, 0, 0, 0 (0 / 0 / 0); 48, 536.2 mm, 19.2 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 51.8 s | 3, 3, 3, 3 (3 / 3 / 3); 18, 2210.5 mm, 58.4 s |
| corne-cherry | 2 | 3, 4, 0, 3 (0 / 3 / 4); 484, 8273.0 mm, 28.9 s | 2, 1, 0, 0 (0 / 0.5 / 2); 625, 8906.3 mm, 58.4 s |
| tinytapeout-demo | 4 | 15, 3, 15, 2 (2 / 9 / 15); 238, 4930.2 mm, 60.2 s | 0, 4, 0, 4 (0 / 2 / 4); 427, 5610.8 mm, 58.3 s cut |
| buspirate5-rev10 | 4 | 4, 2, 12, 5 (2 / 4.5 / 12); 314, 3044.6 mm, 30.4 s | 0, 0, 0, 0 (0 / 0 / 0); 368, 3469.5 mm, 46.4 s cut |
| lumenpnp-mobo | 4 | 13, 9, 4, 33 (4 / 11 / 33); 340, 5370.3 mm, 62.0 s cut | 0, 0, 0, 0 (0 / 0 / 0); 408, 6292.1 mm, 53.2 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 62.2 s cut | 12, 11, 12, 11 (11 / 11.5 / 12); 643, 3959.9 mm, 55.5 s cut |
| glasgow-revC3 | 4 | 36, 37, 35, 31 (31 / 35.5 / 37); 415, 3436.4 mm, 60.2 s cut | 35, 33, 31, 34 (31 / 33.5 / 35); 392, 3650.4 mm, 58.9 s cut |
| glasgow-revD1 | 6 | 241, 243, 238, 250 (238 / 242 / 250); 898, 7744.3 mm, 63.6 s cut | 122, 135, 118, 134 (118 / 128 / 135); 1253, 8718.3 mm, 57.6 s cut |
| all twelve | | 343, 330, 336, 355 (330 / 339.5 / 355) | 174, 187, 164, 186 (164 / 180 / 187) |
| the eleven without glasgow-revD1 | | 102, 87, 98, 105 (87 / 100 / 105) | 52, 52, 46, 52 (46 / 52 / 52) |

DRC errors other than unrouted, all runs: 0; longest call 63.6 s

Pads in pours after the same runs (`CADLAB_BENCH_THERMALS=1`, `board::thermal::census`), the twelve boards together,
by run:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2345 to 2354 | 36, 45, 38, 36 | 154, 162, 162, 156 | 431 to 441 |
| guide 0.0.16, four orders (D110) | 2355 to 2363 | 2, 2, 2, 2 | 91, 82, 89, 100 | 442 to 461 |
| guide 0.0.17, four orders (D113) | 2361 to 2366 | 1, 1, 1, 3 | 97, 91, 88, 97 | 459 to 468 |
| guide 0.0.18, four orders | 2350 to 2367 | 1, 1, 1, 3 | 100, 94, 90, 95 | 458 to 467 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2 against 1 to 2; corne-cherry 0 to 2 against 2 to 4; tinytapeout-demo 6 to 10 against 16 to 21; buspirate5-rev10 33 to 43 against 38 to 44; lumenpnp-mobo 3 to 5 against 2 to 3; cynthion 2 to 4 against 2; glasgow-revD1 38 to 42 against 85 to 99.

What the numbers say:

- *Against the native router, by the ranges of the table*: level on buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2; ahead in every
  run on buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 4 to 33; cynthion 11 to 12 against 28 to 29; glasgow-revD1 118 to 135 against 238 to 250; ranges that overlap on corne-cherry 0 to 2 against 0 to 4; tinytapeout-demo 0 to 4 against 2 to 15; glasgow-revC3 31 to 35 against 31 to 37; behind in every run on
  none. The twelve boards: 164 to 187 (median 180) against 330 to 355 (339.5); the
  eleven without glasgow-revD1: 46 to 52 (52) against 87 to 105 (100).
- *From 0.0.17 to 0.0.18* (D113's table for the first, another hour): nrfmicro 0 to 1 open → 0, corne-cherry
  5 to 10 → 0 to 2, tinytapeout-demo 0 to 4 → 0 to 4, buspirate5-rev10 0 → 0, cynthion 11 to 12 → 11 to 12,
  glasgow-revC3 31 to 35 → 31 to 35, lumenpnp-mobo 0 → 0, sweep-v2.2 3 → 3, glasgow-revD1 118 to 134 → 118 to 135.
  At 40 million nodes over three orders on the nine boards a budget cuts: 715 → 710. At 150 million nodes, two
  orders: corne-cherry 3 and 3 → 2 and 1, tinytapeout-demo 0 and 0 → 0 and 2, cynthion 11 and 9 → 11 and 11,
  glasgow-revC3 28 and 27 → 30 and 28, buspirate5-rev10 and lumenpnp-mobo 0 as before.
- *Vias and track*: vias, the range of the four runs, lumenpnp-mobo 402 to 411 against 309 to 343, tinytapeout-demo 385 to 427 against 238 to 290, sweep-v2.2 18 to 24 against 21 to 28, glasgow-revC3 379 to 403 against 415 to 439, buspirate5-rev10 349 to 372 against 291 to 314, cynthion 604 to 699 against 469 to 482, corne-cherry 534 to 625 against 445 to 505, glasgow-revD1 1202 to 1253 against 877 to 932. Track, first run:
  lumenpnp-mobo 6292 mm against 5370, tinytapeout-demo 5611 mm against 4930, sweep-v2.2 2210 mm against 2161, glasgow-revC3 3650 mm against 3436, corne-cherry 8906 mm against 8273, cynthion 3960 mm against 3177. The targets were lumenpnp-mobo and tinytapeout-demo within 15 % of the native router's vias, and neither
is met: lumenpnp-mobo 402 to 411 against 309 to 343 (409 to 529 with 0.0.17; some 20 % over the native router's
median), tinytapeout-demo 385 to 427 against 238 to 290 (379 to 494). What went are stubs: lumenpnp-mobo's first run ends with
some 130 stubs to its planes for 254, the same in every order now, where with 0.0.17 the pass of D108 had the
time for it in one order of four. What is left are the vias of the other nets, some 280 on lumenpnp-mobo where the
native router has some 207 in all its nets but those with planes. No pass after routing that shortens or takes out
the vias of those nets was added: guide's own (its negotiation's pass for fewer vias, its cleanup) are as they were.
Track is as before, 6 to 17 % over the native router's.
- *corne-cherry*, the board that was behind: what it had open were pads that cadlab's fill left off their pours
  and guide's cells had told joined. guide now has that fill while it routes, and the wires of its negotiation
  that ended where the fill is no more are routed again (D114).

**Native router and guide 0.0.17, D113** (guide 0.0.17 from crates.io behind the adapter that turns on its
negotiation for every net at once in the first run and gives it the spare spokes; the fill of D112; the corpus at
the default 60 s; four runs of the native router, seeds 1 to 4, three threads, and four orders of guide, three
threads for its negotiation and one for the rest, two runs of each at a time, side by side in the same half hour):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.17: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 5, 497.1 mm, 0.4 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 358.6 mm, 0.4 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 4, 210.1 mm, 0.4 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 11.1 s | 0, 0, 0, 1 (0 / 0 / 1); 48, 536.2 mm, 18.3 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 52.1 s | 3, 3, 3, 3 (3 / 3 / 3); 18, 2210.4 mm, 58.0 s |
| corne-cherry | 2 | 3, 4, 0, 3 (0 / 3 / 4); 484, 8273.0 mm, 29.0 s | 5, 8, 10, 5 (5 / 6.5 / 10); 617, 8738.4 mm, 57.1 s |
| tinytapeout-demo | 4 | 14, 3, 15, 2 (2 / 8.5 / 15); 240, 4932.7 mm, 60.8 s | 4, 1, 0, 3 (0 / 2 / 4); 494, 5592.4 mm, 58.7 s cut |
| buspirate5-rev10 | 4 | 4, 2, 12, 5 (2 / 4.5 / 12); 314, 3044.6 mm, 29.3 s | 0, 0, 0, 0 (0 / 0 / 0); 388, 3486.3 mm, 46.3 s cut |
| lumenpnp-mobo | 4 | 13, 9, 4, 11 (4 / 10 / 13); 340, 5370.3 mm, 62.0 s cut | 0, 0, 0, 0 (0 / 0 / 0); 414, 6300.1 mm, 35.5 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 62.0 s cut | 12, 11, 11, 11 (11 / 11 / 12); 649, 3965.0 mm, 58.9 s cut |
| glasgow-revC3 | 4 | 36, 31, 35, 31 (31 / 33 / 36); 415, 3436.4 mm, 61.7 s cut | 35, 33, 31, 34 (31 / 33.5 / 35); 382, 3654.9 mm, 58.6 s cut |
| glasgow-revD1 | 6 | 239, 243, 239, 250 (239 / 241 / 250); 909, 7853.1 mm, 62.2 s cut | 122, 134, 118, 134 (118 / 128 / 134); 1248, 8757.8 mm, 57.6 s cut |
| all twelve | | 340, 324, 337, 333 (324 / 335 / 340) | 181, 190, 173, 191 (173 / 185.5 / 191) |
| the eleven without glasgow-revD1 | | 101, 81, 98, 83 (81 / 90.5 / 101) | 59, 56, 55, 57 (55 / 56.5 / 59) |

DRC errors other than unrouted, all runs: 0; longest call 66.3 s

Pads in pours after the same runs (`CADLAB_BENCH_THERMALS=1`, `board::thermal::census`), the twelve boards together,
by run:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2345 to 2353 | 36, 45, 38, 35 | 154, 162, 162, 155 | 432 to 442 |
| guide 0.0.16, four orders (D110) | 2355 to 2363 | 2, 2, 2, 2 | 91, 82, 89, 100 | 442 to 461 |
| guide 0.0.17, four orders | 2361 to 2366 | 1, 1, 1, 3 | 97, 91, 88, 97 | 459 to 468 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2 against 1 to 2; corne-cherry 0 to 2 against 2 to 4; tinytapeout-demo 8 to 11 against 16 to 21; buspirate5-rev10 31 to 38 against 38 to 44; lumenpnp-mobo 3 to 5 against 1 to 3; cynthion 1 to 4 against 2; glasgow-revD1 38 to 42 against 85 to 99.

What the numbers say:

- *Against the native router, by the ranges of the table*: level on buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, sweep-v2.2; ahead in every
  run on buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 4 to 13; cynthion 11 to 12 against 28 to 29; glasgow-revD1 118 to 134 against 239 to 250; ranges that overlap on nrfmicro 0 to 1 against 0; tinytapeout-demo 0 to 4 against 2 to 15; glasgow-revC3 31 to 35 against 31 to 36; behind in every run on
  corne-cherry 5 to 10 against 0 to 4. The twelve boards: 173 to 191 (median 185.5) against 324 to 340 (335); the
  eleven without glasgow-revD1: 55 to 59 (56.5) against 81 to 101 (90.5).
- *From 0.0.16 to 0.0.17* (D110's table for the first, another hour): nrfmicro 0 to 2 open → 0 to 1, corne-cherry 15
  to 21 → 5 to 10, tinytapeout-demo 2 to 10 → 0 to 4, buspirate5-rev10 0 to 4 → 0, cynthion 10 to 22 → 11 to 12,
  glasgow-revC3 31 to 36 → 31 to 35, lumenpnp-mobo 0 → 0, sweep-v2.2 3 → 3, glasgow-revD1 116 to 135 → 118 to 134
  (that board is given up by the negotiation and routed as before). At 40 million nodes over three orders on the
  nine boards a budget cuts: 732 → 715. At 150 million nodes, two orders: corne-cherry 3 and 3, tinytapeout-demo 0
  and 0, buspirate5-rev10 0 and 0, lumenpnp-mobo 0 and 0, cynthion 11 and 9, glasgow-revC3 28 and 27.
- *Vias and track*: vias, the range of the four runs, lumenpnp-mobo 409 to 529 against 336 to 343, tinytapeout-demo 379 to 494 against 240 to 290, sweep-v2.2 18 to 24 against 21 to 28, glasgow-revC3 375 to 388 against 415 to 439, buspirate5-rev10 385 to 410 against 291 to 314, cynthion 577 to 703 against 469 to 482, corne-cherry 518 to 623 against 445 to 505, glasgow-revD1 1202 to 1248 against 877 to 929. Track, first run:
  lumenpnp-mobo 6300 mm against 5370, tinytapeout-demo 5592 mm against 4933, sweep-v2.2 2210 mm against 2161, glasgow-revC3 3655 mm against 3436, corne-cherry 8738 mm against 8273, cynthion 3965 mm against 3177.
- *corne-cherry* is the board still behind. What it has open are pads of its grounds and of a supply that
  cadlab's fill leaves cut off from their pours after guide's first run, which guide's own estimate of the fill
  had told joined (482 of 482 connections on its board): the native router fills the zones itself between two
  negotiations (D80), and guide's pour rounds have its cells (D113, "Gaps").

**Native router and guide 0.0.16, D110** (guide 0.0.16 from crates.io behind the adapter that gives it the
spokes of the fill and a price of 4 on their room; the fill of D112; the corpus at the default 60 s; four runs of
the native router, seeds 1 to 4, three threads, two at a time, and then four orders of guide, one thread, four
at a time, with a check of two threads beside the second):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.16: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 7, 498.3 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 357.0 mm, 0.8 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 1, 176.5 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 7, 212.4 mm, 0.5 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 11.1 s | 2, 0, 1, 0 (0 / 0.5 / 2); 52, 550.5 mm, 59.0 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 54.5 s | 3, 3, 3, 3 (3 / 3 / 3); 42, 2336.5 mm, 58.4 s |
| corne-cherry | 2 | 3, 4, 0, 3 (0 / 3 / 4); 484, 8273.0 mm, 29.5 s | 21, 15, 20, 20 (15 / 20 / 21); 554, 6842.6 mm, 59.3 s cut |
| tinytapeout-demo | 4 | 15, 3, 15, 2 (2 / 9 / 15); 238, 4930.2 mm, 61.2 s | 5, 7, 2, 10 (2 / 6 / 10); 598, 5812.6 mm, 58.9 s cut |
| buspirate5-rev10 | 4 | 4, 2, 12, 5 (2 / 4.5 / 12); 314, 3044.6 mm, 30.9 s | 0, 4, 0, 1 (0 / 0.5 / 4); 408, 3729.8 mm, 53.2 s cut |
| lumenpnp-mobo | 4 | 13, 9, 4, 33 (4 / 11 / 33); 340, 5370.3 mm, 62.0 s cut | 0, 0, 0, 0 (0 / 0 / 0); 620, 6553.7 mm, 38.6 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3176.6 mm, 62.1 s cut | 12, 10, 22, 14 (10 / 13 / 22); 536, 3537.7 mm, 57.5 s cut |
| glasgow-revC3 | 4 | 36, 31, 35, 31 (31 / 33 / 36); 415, 3436.4 mm, 60.0 s cut | 34, 31, 36, 32 (31 / 33 / 36); 559, 4011.1 mm, 58.1 s cut |
| glasgow-revD1 | 6 | 241, 242, 288, 250 (241 / 246 / 288); 898, 7744.3 mm, 63.4 s cut | 123, 135, 116, 116 (116 / 119.5 / 135); 1284, 8777.6 mm, 57.6 s cut |
| all twelve | | 343, 323, 386, 355 (323 / 349 / 386) | 200, 205, 200, 196 (196 / 200 / 205) |
| the eleven without glasgow-revD1 | | 102, 81, 98, 105 (81 / 100 / 105) | 77, 70, 84, 80 (70 / 78.5 / 84) |

Zero DRC errors other than unrouted on every run, no via in a pad of its net, no call of guide over 59.5 s (the
native router's longest 64.9 s). In none of guide's 48 runs did the verification take copper out: the via on a
via of D108 (buspirate5-rev10, the order of seed 3) is gone with 0.0.16.

Pads in pours after the same runs (`CADLAB_BENCH_THERMALS=1`, `board::thermal::census`), the twelve boards
together, by run:

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| native router, seeds 1 to 4 | 2345 to 2354 | 36, 45, 49, 36 | 154, 162, 170, 156 | 431 to 441 |
| guide 0.0.15, two orders (the adapter of D108 on this fill) | 2355, 2359 | 1, 3 | 138, 128 | 426, 433 |
| guide 0.0.16, four orders | 2355 to 2363 | 2, 2, 2, 2 | 91, 82, 89, 100 | 442 to 461 |

Short of two necks by board, guide 0.0.16 in its four orders against the native router in its four seeds:
buspirate5-rev10 26 to 35 against 38 to 44, glasgow-revD1 38 to 41 against 85 to 99, tinytapeout-demo 6 to 11
against 16 to 21, lumenpnp-mobo 5 to 13 against 2 to 3, cynthion 1 to 3 against 2, corne-cherry 0 to 3 against 2
to 4, sweep-v2.2 0 to 1 against 1 to 2. As routed by their projects the twelve boards have 116 (D112). The solid
pads are the zones' own setting, as before; more of them are counted with guide 0.0.16 because more pads are
left in their pours.

What the numbers say:

- *From 0.0.15 to 0.0.16*, on this fill, two orders at 60 s: 238 and 218 open on the twelve boards → 200 and
  205; pads short of two necks 138 and 128 → 91 and 82. At 40 million nodes over three orders on the nine boards
  a budget cuts: 782 → 732 (corne-cherry 105 → 75, sweep-v2.2 16 → 9, glasgow-revD1 454 → 433, cynthion 56 → 53, tinytapeout-demo 33 → 35, buspirate5-rev10 12 → 16, nrfmicro 2 → 5, glasgow-revC3 104 → 106, lumenpnp-mobo 0 → 0).
- *Against the native router, by the ranges of the table*: level on the three small boards and sweep-v2.2
  (3 in every run of both, two of guide's three told as what the board keeps apart); ahead in every run on
  three: lumenpnp-mobo 0 against 4 to 33, cynthion 10 to 22 against 28 to 29, glasgow-revD1 116 to 135 against
  241 to 288 (the native router had 118 to 200 on it in D108's hour; its four runs here were alone on the
  machine's share, on the new fill); ranges that overlap on four: buspirate5-rev10 0 to 4 against 2 to 12,
  tinytapeout-demo 2 to 10 against 2 to 15 (medians 6 and 9), glasgow-revC3 31 to 36 against 31 to 36, nrfmicro
  0 to 2 against 0 (two orders of four level); behind in every run on one, corne-cherry, 15 to 21 against 0 to
  4. The twelve boards: 196 to 205 (median 200) against 323 to 386 (349); the eleven without glasgow-revD1: 70
  to 84 (78.5) against 81 to 105 (100).
- *Vias and track*, first run: lumenpnp-mobo 620 vias against 340 (511, 686 and 674 in the other orders),
  tinytapeout-demo 598 against 238, buspirate5-rev10 408 against 314, glasgow-revC3 559 against 415,
  glasgow-revD1 1284 against 898, sweep-v2.2 42 against 21, cynthion 536 against 476. Track is longer than the
  native router's on eleven boards, by 8% (sweep-v2.2, buspirate-flash-sop) to 27% (buspirate-rs232), and
  shorter on corne-cherry, where it leaves more open. Nothing in 0.0.16 is for either (D110, "Tried and not
  kept").

**Native router and guide 0.0.15, D108** (guide 0.0.15 from crates.io behind the adapter with its runs in other
orders and the needless stubs taken out; the corpus at the default 60 s; four runs of the native router, seeds 1
to 4, three threads, two at a time, and four orders of guide, one thread, four at a time, begun 15 minutes
apart in one hour, with a build of two threads beside the second):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.15: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 7, 496.9 mm, 0.8 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 356.2 mm, 0.7 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 2, 180.4 mm, 0.4 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 209.6 mm, 0.3 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 11.3 s | 1, 1, 0, 0 (0 / 0.5 / 1); 55, 538.2 mm, 56.8 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 52.9 s | 3, 3, 3, 3 (3 / 3 / 3); 48, 2290.1 mm, 57.7 s cut |
| corne-cherry | 2 | 3, 4, 0, 3 (0 / 3 / 4); 483, 8275.6 mm, 28.6 s | 15, 22, 19, 23 (15 / 20.5 / 23); 526, 6911.1 mm, 58.9 s cut |
| tinytapeout-demo | 4 | 6, 3, 6, 3 (3 / 4.5 / 6); 274, 4913.3 mm, 58.6 s | 12, 5, 0, 16 (0 / 8.5 / 16); 570, 5847.8 mm, 56.3 s cut |
| buspirate5-rev10 | 4 | 2, 3, 5, 0 (0 / 2.5 / 5); 313, 3053.2 mm, 27.4 s | 0, 0, 1, 0 (0 / 0 / 1); 452, 3762.0 mm, 37.4 s |
| lumenpnp-mobo | 4 | 9, 6, 3, 17 (3 / 7.5 / 17); 344, 5425.7 mm, 62.2 s cut | 0, 0, 0, 0 (0 / 0 / 0); 706, 6523.7 mm, 35.1 s |
| cynthion | 4 | 28, 29, 29, 28 (28 / 28.5 / 29); 476, 3177.0 mm, 61.3 s cut | 12, 14, 14, 11 (11 / 13 / 14); 530, 3547.5 mm, 53.9 s cut |
| glasgow-revC3 | 4 | 36, 36, 35, 31 (31 / 35.5 / 36); 415, 3436.4 mm, 60.6 s cut | 33, 31, 35, 34 (31 / 33.5 / 35); 558, 4014.2 mm, 57.8 s cut |
| glasgow-revD1 | 6 | 200, 137, 118, 142 (118 / 139.5 / 200); 1029, 8897.0 mm, 61.2 s cut | 162, 135, 123, 120 (120 / 129 / 162); 1085, 7953.6 mm, 57.9 s cut |
| all twelve | | 287, 221, 199, 227 (199 / 224 / 287) | 238, 211, 195, 207 (195 / 209 / 238) |
| the eleven without glasgow-revD1 | | 87, 84, 81, 85 (81 / 84.5 / 87) | 76, 76, 72, 87 (72 / 76 / 87) |

Zero DRC errors other than unrouted on every run, no via in a pad of its net, no call of guide over 59.1 s. In
one run of the 48 the verification took copper out: buspirate5-rev10 in the order of seed 3, where a run after
the first put a via of GND on a via of GND (`drc.hole_to_hole`, 15 items of the net taken out, the one connection
that order has open). guide 0.0.13 did the same in that order at 40 million nodes; it is guide's to mend.

What the numbers say:

- *From 0.0.14 to 0.0.15*, the two from source behind D106's adapter, one after the other: 902 open over the four
  orders on the twelve boards → 877; nrfmicro 5, 2, 3 and 3 → 1, 1, 1 and 3, corne-cherry 15, 27, 19 and 26 → 15,
  22, 19 and 23, buspirate5-rev10 0, 2, 3 and 0 → 0, 1, 1 and 0, the others within one of themselves. With this
  adapter and the crate from crates.io, the table: 851.
- *Against the native router, by the ranges of the table*: level on the three small boards and sweep-v2.2;
  ahead in every run on two (lumenpnp-mobo 0 against 3 to 17, cynthion 11 to 14 against 28 to 29); ranges that
  overlap on five: nrfmicro 0 to 1 against 0 (two orders of four level, where every order had 2 to 5),
  buspirate5-rev10 0 to 1 against 0 to 5, glasgow-revC3 31 to 35 against 31 to 36, glasgow-revD1 120 to 162
  against 118 to 200, tinytapeout-demo 0 to 16 against 3 to 6, medians 8.5 and 4.5; behind in every run on one,
  corne-cherry, 15 to 23 against 0 to 4. The twelve boards: 195 to 238 (median 209) against 199 to 287 (224);
  the eleven without glasgow-revD1: 72 to 87 (76) against 81 to 87 (84.5).
- *Vias and track*, first run: lumenpnp-mobo 706 vias against 344 in the order with no seed, where the stubs
  taken out left more open and were put back, and 510, 571 and 568 in the three others (699, 738 and 720 with
  0.0.14); buspirate5-rev10 452 against 313 (484); tinytapeout-demo 570 against 274 (605), and 450 in the order
  that ends before its deadline; glasgow-revC3 558 against 415, cynthion 530 against 476. Track is longer than the
  native router's on ten boards, by 6% (sweep-v2.2) to 27% (buspirate-rs232), and shorter on corne-cherry,
  where it has less routed, and on glasgow-revD1.
- *At 40 million nodes*, three orders, the nine boards a budget cuts: 797 open with 0.0.14 → 791 (nrfmicro 5, 2
  and 3 → 1, 1 and 0; lumenpnp-mobo 0, 1 and 0 → 0, 0 and 0; tinytapeout-demo 16, 19 and 1 → 16, 19 and 0;
  corne-cherry 38, 38 and 32 → 39, 41 and 32; sweep-v2.2 16, buspirate5-rev10 12, cynthion 56, glasgow-revC3 104
  and glasgow-revD1 454 the same). A run that the budget stops is the same from source and from crates.io, to
  the connection (corne-cherry 39, 41 and 32 in both).

The default is unchanged (DECISIONS D108).

**Native router and guide 0.0.14, D106** (guide 0.0.14's source behind the adapter with three runs; the corpus at
the default 60 s; four runs of the native router, seeds 1 to 4, three threads, two at a time, and four orders of
guide, one thread, four at a time, side by side in one hour, ten threads of this work beside the machine's other
load):

| board | layers | native: open by run (min / median / max); vias, length, time | guide 0.0.14: open by run (min / median / max); vias, length, time |
|---|---|---|---|
| buspirate-flash-sop | 2 | 0, 0, 0, 0 (0 / 0 / 0); 4, 460.2 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 7, 496.9 mm, 0.9 s |
| buspirate-rs232 | 2 | 0, 0, 0, 0 (0 / 0 / 0); 10, 280.9 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 6, 356.2 mm, 0.8 s |
| lumenpnp-ringlight | 2 | 0, 0, 0, 0 (0 / 0 / 0); 2, 180.4 mm, 0.5 s | 0, 0, 0, 0 (0 / 0 / 0); 1, 209.6 mm, 0.3 s |
| nrfmicro | 2 | 0, 0, 0, 0 (0 / 0 / 0); 46, 453.6 mm, 11.4 s | 5, 2, 3, 3 (2 / 3 / 5); 44, 494.3 mm, 23.5 s |
| sweep-v2.2 | 2 | 3, 3, 3, 3 (3 / 3 / 3); 21, 2160.9 mm, 52.7 s | 3, 4, 4, 4 (3 / 4 / 4); 48, 2290.1 mm, 57.6 s cut |
| corne-cherry | 2 | 3, 4, 0, 3 (0 / 3 / 4); 483, 8275.6 mm, 30.4 s | 15, 27, 19, 26 (15 / 22.5 / 27); 529, 6907.1 mm, 59.0 s cut |
| tinytapeout-demo | 4 | 6, 3, 6, 3 (3 / 4.5 / 6); 274, 4913.3 mm, 61.4 s | 9, 7, 1, 16 (1 / 8 / 16); 605, 5799.4 mm, 58.3 s cut |
| buspirate5-rev10 | 4 | 2, 3, 5, 0 (0 / 2.5 / 5); 313, 3053.2 mm, 29.6 s | 0, 2, 2, 0 (0 / 1 / 2); 484, 3792.0 mm, 37.6 s |
| lumenpnp-mobo | 4 | 15, 8, 4, 24 (4 / 11.5 / 24); 347, 5423.7 mm, 62.5 s cut | 0, 1, 0, 0 (0 / 0 / 1); 712, 6538.5 mm, 32.3 s |
| cynthion | 4 | 29, 32, 29, 28 (28 / 29 / 32); 480, 3206.0 mm, 61.8 s cut | 17, 15, 23, 21 (15 / 19 / 23); 515, 3556.2 mm, 55.1 s cut |
| glasgow-revC3 | 4 | 34, 32, 35, 31 (31 / 33 / 35); 413, 3426.4 mm, 60.8 s cut | 33, 30, 38, 34 (30 / 33.5 / 38); 560, 4014.4 mm, 57.9 s cut |
| glasgow-revD1 | 6 | 192, 207, 115, 194 (115 / 193 / 207); 1046, 8967.0 mm, 60.8 s cut | 162, 139, 125, 125 (125 / 132 / 162); 1085, 7953.6 mm, 57.7 s cut |
| all twelve | | 284, 292, 197, 286 (197 / 285 / 292) | 244, 227, 215, 229 (215 / 228 / 244) |
| the eleven without glasgow-revD1 | | 92, 85, 82, 92 (82 / 88.5 / 92) | 82, 88, 90, 104 (82 / 89 / 104) |

Zero DRC errors other than unrouted on every run, no via in a pad of its net, nothing taken out by the
verification; no call of guide over 59.1 s.

What the numbers say:

- *From 0.0.13 to 0.0.14*, the two measured one after the other with six runs at a time and nothing else of
  this work running (0.0.13 behind the adapter with two runs → 0.0.14's source behind the one with three): the
  twelve boards 259, 227, 218 and 219 open → 241, 223, 205 and 221, 923 → 890 over the four orders (the eleven
  without glasgow-revD1 97, 91, 94 and 98 → 79, 87, 81 and 97). corne-cherry
  33, 27, 21 and 26 → 15, 27, 19 and 26; buspirate5-rev10 0, 5, 4 and 0 → 0, 1, 0 and 0; tinytapeout-demo 9, 7,
  5 and 16 → 9, 7, 1 and 16; cynthion 14, 15, 23 and 15 → 14, 15, 20 and 15; lumenpnp-mobo 0, 1, 0 and 1 → 0,
  1, 0 and 0; nrfmicro, sweep-v2.2 (3 in every order) and glasgow-revC3 the same; glasgow-revD1 three more in
  one order, in its first run. A third run is only kept if the board has less open for it.
- *Against the native router, by the ranges of the table*: level on the three small boards; ahead in every run
  on two (lumenpnp-mobo 0 to 1 against 4 to 24, cynthion 15 to 23 against 28 to 32); ranges that overlap on four
  (buspirate5-rev10 0 to 2 against 0 to 5, medians 1 and 2.5; glasgow-revC3 30 to 38 against 31 to 35;
  glasgow-revD1 125 to 162 against 115 to 207, medians 132 and 193; tinytapeout-demo 1 to 16 against 3 to 6,
  medians 8 and 4.5); behind in every run on two (nrfmicro 2 to 5 against 0, corne-cherry 15 to 27 against 0 to
  4). sweep-v2.2 has 3, 4, 4 and 4 against 3 in this hour, and 3 in each of the four orders with the released
  crate and four runs at a time: its second run is cut by the deadline while it joins the last pad, and ten
  threads of other runs beside it are enough to lose that. The twelve boards: 215 to 244 (median 228) against
  197 to 292 (285); the eleven without glasgow-revD1: 82 to 104 (89) against 82 to 92 (88.5).
- *Vias and track*, first run, guide 0.0.14 against the native router: lumenpnp-mobo 712 vias against 347,
  tinytapeout-demo 605 against 274, buspirate5-rev10 484 against 313, glasgow-revC3 560 against 413, sweep-v2.2
  48 against 21, corne-cherry 529 against 483, cynthion 515 against 480, glasgow-revD1 1085 against 1046,
  nrfmicro 44 against 46. Nothing in this release is for vias.
- *At a budget of work*, 40 million of guide's nodes, three orders: a run that the budget stops is the same to
  the track as with 0.0.13 (corne-cherry 38, 38 and 32 with the released crate; cynthion 14, 17 and 25), and
  after one that ends within it the runs after go on within the budget: tinytapeout-demo 16, 19 and 1 for 16, 19
  and 5, nrfmicro 5, 2 and 3 as before.
- *The negotiations for places* (`RouteOptions::negotiate_places`, which the adapter leaves off): with a budget
  of four minutes, corne-cherry's runs end on their own after 114 to 142 s with 9 and 11 connections open in two
  orders with them (8 in an earlier build) for 13 and 16 without. At 60 s they are no gain: corne-cherry 71, 84
  and 87 over four orders in three builds for 87 without, cynthion 75 for 64, sweep-v2.2 14 for 12, nrfmicro 10
  for 13.

The default is unchanged (DECISIONS D106).

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

## freerouting on the corpus (D102)

The twelve boards of the open-source corpus routed by freerouting, counted by cadlab, beside the native router and
guide 0.0.9 (measured 2026-11-17). freerouting is GPL-3.0 and is run as a separate process on a released jar,
nothing else (the licensing rule at the top of this file).

**How it is run** (`examples/route_bench.rs`, corpus mode with `CADLAB_BENCH_ROUTER=freerouting`). Each board is
imported as for the native router and guide (circuit built from the board, the project's rules), its tracks and
vias are removed, and the design `export.dsn` writes is given to freerouting; the session it writes is applied as
`route.import_ses` applies it, the pours are filled again, and the result is counted by the code that counts the
other two: open connections are `drc.unrouted` of cadlab's DRC, DRC errors are the errors the routed board has and
the stripped board had not, vias and track length are those of the board, time is the wall time of the process
(JVM start, reading the design and writing the session included, 2 to 4 s of it).

- *freerouting 2.5.0*, `freerouting-2.5.0.jar` from the project's GitHub release `v2.5.0` (2026-10-05), SHA-256
  `f6f51bb02245e8e717f9359bd260cc9c5c0b1bc0acc8b7cb2cd5b8ffeb5de3c7`. It is built for Java 25 (class file version
  69: Java 21 refuses to load it); run here on Temurin 25.0.4, in CI on Temurin 25.0.5. The tables above this
  section have 2.4.1.
- *Command:* `java -XX:ActiveProcessorCount=<n> -Djava.awt.headless=true -jar freerouting-2.5.0.jar -de board.dsn
  -do board.ses -mt <n> --gui.enabled=false -da --user_data_path=<dir> --router.result_json=<dir>/result.json
  --router.job_timeout=<hh:mm:ss>`, with the JVM's home and temporary directory in the run's own directory, so no
  settings file of the user's is read; `-da` sends no usage data.
- *Settings:* its own defaults, read back from the settings it records in `result.json`: its fanout stage on (20
  passes at most), its routing stage with no limit of passes (`-mp` is not given), its optimizer on (100 passes at
  most, stopping below 2.5 % improvement), neck-down on, vias in pads off (the DSN says `via_at_smd off`, as the
  other two routers have it). The one thing set is the time: `--router.job_timeout`, freerouting's own limit for
  the three stages together. It writes its session when the limit ends a stage, so a cut run has a result.
- *Threads:* `<n>` is 1 here (a shared machine, four threads for everything) and 4 in CI. freerouting's log says
  of every run "autorouter pass is single-threaded": `-mt` sizes the pool of its optimizer, which runs only on a
  board it has routed completely.
- *Two time budgets.* 60 s, the budget of the tables of D88 to D97 and of a command's default; and 30 minutes, with
  2 hours on three of the boards that cut (cynthion and glasgow-revC3, which had made two routing passes, and
  glasgow-revD1, which had not left its fanout stage), so that what it does with time is seen. A run that ends
  on its own before the limit is marked by its time; *cut* marks one the limit ended.
- *Two designs.* The DSN as `export.dsn` writes it by default, without pours, and the DSN with the pours as planes
  (`planes`, above), since a router that is not told of a ground pour has to wire the ground.

**What freerouting did not see.** A DSN has places for the outline, keep-outs, pads, nets, and a width and a
clearance per net class. The rest of what cadlab's DRC checks is not in it, and the count keeps the two apart: an
error of the routed board is *carried* when the DSN held the rule it breaks, *not carried* when it did not
(`not_in_dsn` in the example).

| not in the DSN | on these boards | what the count does with it |
|---|---|---|
| pours (without `planes`) | 1 to 925 zones on eleven boards (nrfmicro has none) | freerouting wires those nets pad to pad: 655 connections on buspirate5-rev10 where cadlab's count, with the pours filled, has 473 to make. What it leaves open is counted on the board with its pours filled again, so a pad the pour reaches is connected whether freerouting wired it or not |
| pours as fills (with `planes`) | the outline of each pour, without priorities, clearances, thermal reliefs or islands | freerouting takes a pad of the net as connected where the plane's outline covers it; cadlab's fill then reaches some of them and not others. Both counts are given |
| the minimum track width | 0.09 to 0.2 mm | freerouting lays track narrower than the class width it is given, most of it at three quarters of it (0.1124 mm for 0.15, 0.1874 for 0.25), and not only as neck-downs at pads: with `--router.automatic_neckdown=false` it lays 723 such tracks for 846. `drc.track_width`, not carried |
| copper to a drilled hole, hole to hole | 0.16 to 0.25 mm, 0.2 to 0.254 mm | `drc.hole_clearance`, `drc.hole_to_hole`, not carried |
| copper to the board edge | 0.025 to 0.5 mm | freerouting's settings record 0.25 mm of its own; `drc.copper_to_edge` where the board asks for more, not carried |
| clearances of single pads and footprints | 3 to 133 pads on eight boards | a clearance error whose gap satisfies the class clearance the DSN gave is not carried |
| custom rules | tinytapeout-demo: 10 (9 with a clearance, 2 with a track width, 1 forbidding items in an area) | as above; the rule areas that forbid nothing by themselves are left out of the DSN (22 on five boards) |
| copper drawings | 18 of the boards and 11 of footprints, on five boards | shorts and clearance errors with a drawing are not carried |
| a keep-out's exception for some nets, differential pairs, length rules | none on these boards | |

Carried, and so freerouting's own: the outline and cutouts, keep-outs for tracks and vias, every pad's copper, the
nets, each class's width, clearance and via.

**At 60 s, this machine** (64-core Linux x86-64, shared, load 6 to 9; the native router alone on three threads,
seed 1, as in D88 to D97; guide 0.0.9 with no seed and the two freerouting designs each on one thread, the three
at a time). A cell is: open connections; vias, track length, wall time. For freerouting: open connections
(freerouting's own count); DRC errors carried + not carried; vias, track length, wall time. *cut*: the budget or
freerouting's limit ended the run. The native router and guide have no DRC error other than unrouted on any board.

| board | layers | native, 3 threads | guide 0.0.9 | freerouting | freerouting, pours as planes |
|---|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0; 4, 460.2 mm, 0.4 s | 0; 7, 496.9 mm, 0.8 s | 0 (0); 0 + 1; 4, 607.1 mm, 7.2 s | 1 (0); 0 + 1; 4, 533.7 mm, 3.7 s |
| buspirate-rs232 | 2 | 0; 10, 280.9 mm, 0.5 s | 0; 6, 356.2 mm, 0.7 s | 1 (1); 0 + 0; 7, 476.8 mm, 22.9 s | 1 (0); 0 + 0; 5, 370.1 mm, 7.5 s |
| lumenpnp-ringlight | 2 | 0; 2, 180.4 mm, 0.4 s | 0; 1, 209.6 mm, 0.4 s | 0 (0); 0 + 11; 5, 331.5 mm, 18.7 s | 8 (0); 0 + 6; 0, 198.4 mm, 31.1 s |
| nrfmicro | 2 | 0; 46, 453.6 mm, 11.6 s | 4; 62, 499.4 mm, 27.7 s | 4 (4); 13 + 32; 26, 512.9 mm, 21.2 s | 4 (4); 13 + 32; 26, 512.9 mm, 20.7 s |
| sweep-v2.2 | 2 | 3; 21, 2160.9 mm, 53.2 s | 6; 42, 2231.0 mm, 56.0 s cut | 13 (18); 0 + 72; 11, 2175.4 mm, 52.6 s | 7 (4); 0 + 90; 15, 2233.9 mm, 35.3 s |
| corne-cherry | 2 | 1; 483, 8275.6 mm, 30.7 s | 36; 825, 8435.7 mm, 46.7 s cut | 143 (176); 0 + 372; 291, 4431.4 mm, 64.1 s cut | 314 (814); 0 + 222; 300, 1211.3 mm, 72.5 s cut |
| tinytapeout-demo | 4 | 8; 273, 4910.1 mm, 60.0 s | 9; 605, 5799.4 mm, 48.8 s cut | 206 (260); 0 + 24; 130, 1416.0 mm, 62.9 s cut | 340 (218); 1 + 21; 204, 939.2 mm, 78.8 s cut |
| buspirate5-rev10 | 4 | 2; 313, 3053.2 mm, 28.6 s | 2; 501, 3815.8 mm, 38.3 s | 162 (210); 0 + 18; 236, 1891.2 mm, 62.8 s cut | 259 (189); 0 + 14; 263, 1676.0 mm, 74.6 s cut |
| lumenpnp-mobo | 4 | 10; 347, 5423.7 mm, 64.1 s cut | 0; 713, 6544.4 mm, 32.5 s | 22 (26); 0 + 2; 357, 7358.0 mm, 66.0 s cut | 202 (159); 0 + 6; 327, 2058.3 mm, 94.8 s cut |
| cynthion | 4 | 27; 478, 3176.5 mm, 61.4 s cut | 19; 649, 4004.4 mm, 57.8 s cut | 226 (336); 0 + 463; 325, 2256.3 mm, 64.2 s cut | 350 (277); 0 + 366; 327, 1952.7 mm, 99.4 s cut |
| glasgow-revC3 | 4 | 35; 415, 3436.4 mm, 60.6 s cut | 35; 543, 3920.3 mm, 57.9 s cut | 243 (354); 0 + 187; 244, 1899.7 mm, 63.4 s cut | 278 (278); 0 + 124; 178, 1491.2 mm, 68.6 s cut |
| glasgow-revD1 | 6 | 197; 1029, 8897.0 mm, 62.3 s cut | 162; 1085, 7953.6 mm, 58.0 s cut | 627 (691); 1 + 6; 487, 3326.9 mm, 66.2 s cut | 1032 (913); 0 + 4; 384, 2389.8 mm, 98.1 s cut |
| all twelve, open | | 283 | 273 | 1647 | 2796 |
| the eleven without glasgow-revD1, open | | 86 | 111 | 1020 | 1764 |

The native router's own count of failed connections, which is the column D88 to D97 give for it, is 294 here as in
D97 (15 on lumenpnp-mobo where the DRC finds 10 open, 3 on corne-cherry for 1); this table has the DRC's count for
all three. guide's row is D97's first order to the connection (273).

**Given time** (the same machine, load 60 to 100 from other work until the last hour; one thread each). The native
router's column is not the 60 s run: it has the same 30 minutes as a budget, on one thread, on the five boards its
60 s cut, and ended on its own on each of them, at the time given.

| board | layers | freerouting, 30 min | freerouting, 30 min, pours as planes | freerouting, 2 h | native, 30 min, 1 thread |
|---|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0 (0); 0 + 1; 4, 607.1 mm, 20.1 s | 1 (0); 0 + 1; 4, 533.7 mm, 6.0 s |  |  |
| buspirate-rs232 | 2 | 1 (1); 0 + 0; 7, 476.8 mm, 59.0 s | 1 (0); 0 + 0; 5, 370.1 mm, 13.2 s |  |  |
| lumenpnp-ringlight | 2 | 0 (0); 0 + 11; 5, 331.5 mm, 40.4 s | 8 (0); 0 + 6; 0, 198.4 mm, 51.8 s |  |  |
| nrfmicro | 2 | 4 (4); 13 + 32; 26, 512.9 mm, 57.9 s | 4 (4); 13 + 32; 26, 512.9 mm, 37.1 s |  |  |
| sweep-v2.2 | 2 | 13 (18); 0 + 72; 11, 2175.4 mm, 161.6 s | 7 (4); 0 + 90; 15, 2233.9 mm, 65.9 s |  |  |
| corne-cherry | 2 | 77 (78); 0 + 554; 301, 6736.1 mm, 1811.4 s cut | 273 (728); 0 + 187; 162, 1376.3 mm, 1568.1 s |  |  |
| tinytapeout-demo | 4 | 126 (143); 0 + 38; 151, 3314.7 mm, 1821.4 s cut | 134 (90); 0 + 17; 157, 3911.3 mm, 1820.7 s cut |  | 6; 274, 4913.3 mm, 115.5 s |
| buspirate5-rev10 | 4 | 94 (104); 0 + 19; 170, 3078.8 mm, 1806.3 s cut | 163 (104); 0 + 6; 126, 2321.9 mm, 1813.7 s cut |  |  |
| lumenpnp-mobo | 4 | 0 (0); 0 + 4; 345, 8792.0 mm, 1817.4 s cut | 68 (21); 0 + 6; 311, 6311.3 mm, 1586.5 s |  | 1; 366, 5500.6 mm, 1234.6 s |
| cynthion | 4 | 181 (214); 0 + 376; 267, 3400.2 mm, 1814.2 s cut | 267 (197); 0 + 348; 284, 2578.6 mm, 1849.2 s cut | 189 (214); 0 + 386; 284, 3275.0 mm, 7208.8 s cut | 24; 471, 3233.5 mm, 458.9 s |
| glasgow-revC3 | 4 | 134 (137); 0 + 181; 233, 3610.7 mm, 1814.8 s cut | 141 (130); 0 + 156; 259, 2901.3 mm, 1819.9 s cut | 132 (136); 0 + 116; 237, 3516.5 mm, 7209.1 s cut | 30; 453, 3593.9 mm, 824.1 s |
| glasgow-revD1 | 6 | 557 (609); 5 + 1; 611, 3816.1 mm, 1821.0 s cut | 717 (572); 3 + 25; 661, 3544.2 mm, 1872.1 s cut | 309 (343); 3 + 3; 762, 8941.8 mm, 7209.7 s cut | 43; 1159, 9877.2 mm, 1352.2 s |
| all twelve, open | | 1187 | 1784 |  |  |
| the eleven without glasgow-revD1, open | | 630 | 1067 |  |  |

**On a CI runner** (the job that is `freerouting-corpus` since D105, then named `freerouting`; 4 vCPU, nothing else running; freerouting with four processors and
`-mt 4`, the native router on its default threads; one run each;
https://github.com/KarpelesLab/cadlab/actions/runs/44010687386; the runs before and after it,
https://github.com/KarpelesLab/cadlab/actions/runs/44003908234 and
https://github.com/KarpelesLab/cadlab/actions/runs/44031677107, have 300, 295, 1690 and 2954, and 303, 328, 1689
and 2935 in the four columns):

| board | layers | native | guide 0.0.9 | freerouting | freerouting, pours as planes |
|---|---|---|---|---|---|
| buspirate-flash-sop | 2 | 0; 4, 460.2 mm, 0.7 s | 0; 7, 496.9 mm, 1.0 s | 0 (0); 0 + 1; 4, 607.1 mm, 9.3 s | 1 (0); 0 + 1; 4, 533.7 mm, 5.2 s |
| buspirate-rs232 | 2 | 0; 10, 280.9 mm, 0.7 s | 0; 6, 356.2 mm, 0.9 s | 0 (0); 0 + 0; 7, 491.1 mm, 15.8 s | 1 (0); 0 + 0; 5, 370.1 mm, 10.7 s |
| lumenpnp-ringlight | 2 | 0; 2, 180.4 mm, 0.6 s | 0; 1, 209.6 mm, 0.3 s | 0 (0); 0 + 11; 5, 331.5 mm, 23.0 s | 8 (0); 0 + 6; 0, 198.4 mm, 32.5 s |
| nrfmicro | 2 | 0; 46, 453.6 mm, 17.4 s | 4; 62, 499.4 mm, 39.1 s cut | 4 (4); 13 + 32; 26, 512.9 mm, 34.3 s | 4 (4); 13 + 32; 26, 512.9 mm, 33.8 s |
| sweep-v2.2 | 2 | 3; 21, 2160.9 mm, 67.6 s | 6; 42, 2231.0 mm, 55.4 s cut | 13 (18); 0 + 72; 11, 2175.4 mm, 63.2 s cut | 7 (4); 0 + 90; 15, 2233.9 mm, 54.9 s |
| corne-cherry | 2 | 1; 483, 8275.6 mm, 43.7 s | 41; 817, 8415.6 mm, 50.0 s cut | 132 (165); 0 + 377; 292, 4327.0 mm, 66.1 s cut | 319 (823); 0 + 209; 266, 1187.3 mm, 76.9 s cut |
| tinytapeout-demo | 4 | 9; 273, 4909.0 mm, 63.2 s cut | 9; 609, 5810.0 mm, 51.1 s cut | 206 (260); 0 + 24; 130, 1416.0 mm, 63.2 s cut | 339 (217); 1 + 13; 215, 943.9 mm, 78.5 s cut |
| buspirate5-rev10 | 4 | 2; 313, 3053.2 mm, 42.6 s | 7; 461, 3494.2 mm, 52.3 s cut | 178 (217); 0 + 27; 275, 1741.6 mm, 64.0 s cut | 259 (190); 0 + 19; 251, 1649.0 mm, 81.3 s cut |
| lumenpnp-mobo | 4 | 22; 291, 5458.9 mm, 61.4 s cut | 0; 713, 6544.4 mm, 46.5 s | 44 (84); 0 + 2; 355, 6546.5 mm, 65.0 s cut | 202 (167); 0 + 6; 321, 2050.4 mm, 102.7 s cut |
| cynthion | 4 | 29; 480, 3206.0 mm, 62.4 s cut | 33; 580, 3522.9 mm, 52.7 s cut | 223 (340); 0 + 380; 309, 2150.8 mm, 65.1 s cut | 350 (277); 0 + 366; 327, 1952.7 mm, 100.4 s cut |
| glasgow-revC3 | 4 | 40; 412, 3497.4 mm, 62.0 s cut | 48; 523, 3777.5 mm, 58.1 s cut | 248 (358); 0 + 191; 235, 1880.0 mm, 64.7 s cut | 346 (345); 0 + 134; 133, 1277.5 mm, 70.7 s cut |
| glasgow-revD1 | 6 | 203; 1005, 8676.3 mm, 64.2 s cut | 170; 1061, 7642.2 mm, 52.4 s cut | 654 (730); 1 + 5; 425, 3055.5 mm, 68.5 s cut | 1109 (1055); 0 + 1; 356, 1924.7 mm, 100.8 s cut |
| all twelve, open | | 309 | 318 | 1702 | 2945 |
| the eleven without glasgow-revD1, open | | 106 | 148 | 1048 | 1836 |

What the numbers say:

- *At 60 s freerouting is ahead in open connections on no board.* It is level with both on buspirate-flash-sop and
  lumenpnp-ringlight (0 open), and on buspirate-rs232 in CI (0; here 1: its routing stage ended with none open and
  its optimizer, which ran next, gave back a route with one). nrfmicro 4 (native 0, guide 4), sweep-v2.2 13 (3 and
  6). On the seven larger boards it is cut with 22 to 627 open where the native router has 1 to 197 and guide 0 to
  162. In sum 1647 open against 283 and 273 here, 1702 against 309 and 318 on the runner. Under a load of 80 to 100
  on this machine, earlier the same day, the same runs had 2257 (and guide 350): a run that a limit cuts gets as
  far as the processor lets it.
- *Where the 60 s go.* freerouting's first stage is a fanout of every surface-mount pin, and on the larger boards
  that stage alone takes the minute (buspirate5-rev10: 747 of 813 pins escaped at 60.7 s, the routing stage then
  given 1.2 s; one run, under load). With the fanout off (`--router.fanout.enabled=false`) the same board had 243
  open for 187 in runs made one after the other.
- *The pours as planes make it worse, not better* (2796 open for 1647 at 60 s, 1784 for 1187 at 30 minutes).
  freerouting's own count goes down on some boards (buspirate5-rev10 189 for 210, sweep-v2.2 4 for 18, with 7 open
  by cadlab's count for 13: the one board that gains), and cadlab's count goes up: a pad the plane's outline covers
  is connected for freerouting and not for the fill, which keeps its clearances and thermal reliefs and leaves
  islands (lumenpnp-ringlight: 0 open by its count, 8 by cadlab's; lumenpnp-mobo at 30 minutes 21 and 68). Its
  fanout also takes longer with planes, and the run goes up to 50 s over its limit. more than 900 of corne-cherry's 925
  zones are teardrops, which `planes` writes as planes like any other pour; freerouting ends there on its own after 26
  minutes with 728 open by its count.
- *Time does not close the gap.* With 30 minutes freerouting has 1187 open, four times what the native router and
  guide leave at 60 s, and fewer than at 60 s on every board it was cut on. Its own count after each routing pass
  falls for a few passes at most and then moves around its best, which it goes back to: on cynthion 214 open
  after pass 1, 350 to 439 after each of passes 2 to 7, 214 again, and never fewer over two hours and 14 passes;
  glasgow-revC3 136 after pass 1 and 136 after pass 15; tinytapeout-demo 143 after pass 1 and after pass 13;
  buspirate5-rev10 150, 132, 108 and then 104 to 275 over thirteen more; corne-cherry 163, 183, 106, 78 and then 78
  to 163 over ten more. 2 hours end with 189 and 132 open where 30 minutes had 181 and 134. On glasgow-revD1 the
  30 minutes ended in the fanout stage (2265 of 2508 pins escaped) before a routing pass ran; with 2 hours the fanout took 35 minutes, the first routing pass 38 and
  left 343 open by its count, the next two 734 and 774, and the board ends with 309 open, where the native router
  has 197 at 60 s and 43 with 30 minutes.
- *The native router with the same 30 minutes* (one thread) leaves 1, 24, 30, 6 and 43 open on lumenpnp-mobo,
  cynthion, glasgow-revC3, tinytapeout-demo and glasgow-revD1, and stops by itself after 2 to 23 minutes;
  freerouting 0, 181, 134, 126 and 557.
- *Where freerouting is ahead.*
  - *lumenpnp-mobo, given time:* every connection closed after 5.5 minutes of routing (the remaining 24.5 are one
    optimizer pass that changes nothing), where the native router has 10 open at 60 s and 1 after 20.6 minutes.
    guide has 0 there in 32 s. freerouting's board wires the nets the pours carry (8792 mm of track for 5501 and
    6544) and has four tracks under the minimum width.
  - *Vias.* Fewer than guide wherever the two can be compared: 345 for 713 on lumenpnp-mobo with every connection
    closed on both, 26 for 62 on nrfmicro with 4 open on both, 4 for 7 on buspirate-flash-sop. Against the native
    router: 26 for 46 on nrfmicro (with 4 open for 0), 7 for 10 on buspirate-rs232 (1 open for 0), 345 for 347 to
    366 on lumenpnp-mobo, 4 for 4 and 5 for 2 on the two boards all three complete.
  - Nowhere in track length (607 mm for 460 and 497 on buspirate-flash-sop, 332 for 180 and 210 on
    lumenpnp-ringlight: without the pours it wires the ground, and with them 534 and 198 with 1 and 8 open) and
    nowhere in time (7 to 23 s on the three small boards for 0.4 to 0.8 s).
- *What its routes break.* Of the rules the DSN carried: tracks of nrfmicro in the notch of the outline under
  the USB receptacle, 13 or 14 in every run (the boundary written has the notch; the receptacle's pads reach into it);
  on glasgow-revD1 a via on the ring-shaped pad of a mounting stud (written as a polygon with a slit, the one way
  a DSN polygon holds a ring; 1 short at 60 s, 4 and a clearance error at 30 minutes); on tinytapeout-demo with
  planes a via 0.208 mm from a via of a class that asks for 0.25 mm. Of the rules it did not carry, 1188 errors at
  60 s on the twelve boards: 1061 tracks under the minimum width, 119 tracks or vias too close to a drilled hole,
  5 shorts or clearance errors with copper drawings, one track too close to the edge, one hole too close to a
  hole, one track inside a fiducial's own clearance. The native router and guide read the project, not a DSN, and
  have none.

**The generated boards with freerouting 2.5.0** (`route_bench` with `CADLAB_ORACLE_FREEROUTING`, `-mp 20 -mt 1` as
in the table of "Benchmarks", which has 2.4.1 on another machine; this machine, one thread each, the native
router with seed 1; DRC errors other than unrouted):

| board | native: completion, vias, length, time | freerouting 2.5.0: completion, vias, length, time, DRC errors |
|---|---|---|
| attiny-2l | 100 %, 4, 104.1 mm, 0.2 s | 100 %, 2, 141.2 mm, 6.8 s, 0 |
| soic24-2l | 100 %, 141, 1690.6 mm, 1.8 s | 100 %, 145, 2109.5 mm, 27.1 s, 0 |
| stm32-2l | 100 %, 58, 970.4 mm, 0.9 s | 100 %, 41, 1115.7 mm, 82.2 s, 0 |
| stm32-4l | 100 %, 54, 902.1 mm, 2.7 s | 100 %, 44, 1016.0 mm, 89.4 s, 0 |
| qfp-qfn-4l | 100 %, 128, 1934.5 mm, 7.5 s | 100 %, 142, 2258.8 mm, 142.4 s, 110 tracks under the minimum width |
| fine-4l | 100 %, 103, 818.8 mm, 1.4 s | 100 %, 98, 1007.5 mm, 92.8 s, 57 tracks under the minimum width |
| bga144-4l | 100 %, 154, 2211.2 mm, 12.4 s | 95.7 % (6 open), 109, 2325.3 mm, 469.9 s, 111 tracks under the minimum width |

2.5.0 completes six of the seven where 2.4.1 completed three; it has fewer vias than the native router on five
boards (2 for 4, 41 for 58, 44 for 54, 98 for 103, and 109 for 154 with six connections open), 5 to 36 % more
track on all seven, and 15 to 90 times the time.

**To make the tables again.** `scripts/fetch-corpus.sh`, the jar from
`https://github.com/freerouting/freerouting/releases/download/v2.5.0/freerouting-2.5.0.jar` (check the SHA-256
above), a Java 25, then:

```sh
export CADLAB_ORACLE_FREEROUTING=/path/freerouting-2.5.0.jar CADLAB_ORACLE_JAVA=/path/java-25/bin/java
cargo build --release --example route_bench
B=target/release/examples/route_bench
CADLAB_BENCH_ROUTER=freerouting CADLAB_BENCH_VERBOSE=1 $B --corpus                              # 60 s, no pours
CADLAB_BENCH_ROUTER=freerouting CADLAB_BENCH_FR_PLANES=1 $B --corpus                           # pours as planes
CADLAB_BENCH_ROUTER=freerouting CADLAB_BENCH_BUDGET_MS=1800000 $B --corpus                     # 30 minutes
RAYON_NUM_THREADS=3 CADLAB_BENCH_SEED=1 $B --corpus                                            # native, 60 s
RAYON_NUM_THREADS=1 CADLAB_BENCH_ROUTER=guide $B --corpus                                      # guide, 60 s
```

`CADLAB_BENCH_FR_THREADS=<n>` gives freerouting more processors, `CADLAB_BENCH_KEEP=<dir>` keeps each board's DSN,
session, log and `result.json` (and `CADLAB_BENCH_FR_REUSE=1` then counts the kept sessions again without a run),
`CADLAB_BENCH_FR_ARGS` adds arguments, `CADLAB_BENCH_VERBOSE=1` lists each error with its place, each open
connection and what the DSN did not hold. The CI job `freerouting-corpus` runs the 60 s tables on the weekly
schedule and on a manual dispatch (`gh workflow run CI --ref <branch>`; about 46 minutes; not on a push, D105;
docs/TESTING.md) and keeps them in its summary and in the `freerouting-corpus` artifact. Times are budgets, so no two runs give the same numbers: the three CI runs are within
13 connections of each other in 1700 for freerouting, this machine a third apart between a quiet hour and a
loaded one.

**Not established.** One run per cell: the spread of freerouting's results between runs is known only from the three
CI runs and the two loads. freerouting with more than one processor on this machine, and with more than 4 anywhere.
Its settings other than the defaults, beyond the two tried (fanout off on one board, neck-down off on all:
2137 open for 2257 under the same load, 723 tracks under the minimum width for 846). Whether the via on
glasgow-revD1's ring pad is freerouting's reading of the slit polygon or its own error. guide in its three
orders, and guide with more than 60 s, are in D97 and D88 and were not run again.
