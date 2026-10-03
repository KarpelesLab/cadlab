# Board (M4 design)

The physical board: stackup, outline, rules, footprint placement, copper (tracks, vias, zones), keep-outs and
graphics. Stored in `board.json`; zone fills are derived data, recomputed on demand (see "Zone fill").

## Conventions

- Coordinates in `Nm`, Y up, origin at the lower-left of the outline's bounding box by default (any origin is
  valid; outputs translate as their format requires).
- Layer names follow the widespread convention: copper `F.Cu`, `In1.Cu` … `In30.Cu`, `B.Cu`; technical
  `F.SilkS`/`B.SilkS`, `F.Mask`/`B.Mask`, `F.Paste`/`B.Paste`, `F.Fab`/`B.Fab`, `F.CrtYd`/`B.CrtYd`,
  `Edge.Cuts`. These are names only (`crate::board::Layer`), no code is shared with any tool (D7).
- A footprint on the bottom side is mirrored across the Y axis (x → −x) before rotation, and all its layers swap
  F ↔ B.
- Every board item (track, via, zone, keep-out, graphic) has an `ObjectId`; commands address them as
  `track#12`, `via#40`, `zone:GND_bottom`.

## Model (`src/model/board.rs`)

```
Board
├── stackup       copper layer count and names, finished thickness, copper weights, dielectric (optional),
│                 board spec preferences (finish, mask/silk colors) — requirements, not a fab choice (D12)
├── outline       closed contours of line/arc segments: the first is the outer edge, others are cutouts
├── rules         design rules (engineering intent): clearance, track width, via drill/diameter, min annular
│                 ring, min drill, hole-to-hole, copper-to-edge, silk-to-pad, zone min width; net classes override
│                 per net (from the circuit)
├── footprints    by refdes: position, rotation, side, locked; footprint name defaults to the part's
├── tracks        segments (and arcs): layer, width, net, start, end, optional arc midpoint
├── vias          position, drill, diameter, net, layer span (through by default)
├── zones         polygon, layer(s), net, priority, clearance, min width, thermal relief settings
├── keepouts      polygon, layers, what is forbidden (tracks, vias, pours, footprints)
└── graphics      silkscreen/fab/user drawings and texts
```

## Shared geometry (`src/board/`)

One module turns the board into geometry that every consumer uses, so DRC, rendering, Gerber output and the
router agree on what copper exists:

- `pads(board, project)`: every placed pad with its absolute shape (polyclip polygon), layers, net, drill.
- `copper(board, project, layer)`: pads, tracks (stadium shapes), vias, zone fills on a layer, each with its net.
- `ratsnest(board, project)`: unrouted connections per net (minimum spanning tree over pads, minus what copper
  already connects), from the circuit's nets. `ratsnest_items(items)` / `ratsnest_from(items, islands)` do
  the same from copper items a caller already has (DRC and rendering compute the items once).
- `connectivity(board, project)`: copper islands and which nets they join (shorts) or split (opens).
- `prepared::Prepared`: a large shape (zone fill) indexed once, answering `intersects` and
  `distance_less_than` exactly as `polyclip` does but from the edges near the other operand (see
  "Performance").

## Zone fill (`src/board/zones.rs`)

For each zone and each of its layers, in priority order (higher first, ties in board order):

1. Area = zone outline ∩ board outline (outer contour minus cutouts, shrunk by `copper_to_edge`).
2. Minus every other-net copper item on the layer (netless items too) inflated by max(zone clearance, the item's
   net-class clearance); NPTH holes inflated by the zone clearance; higher-priority fills of other nets inflated by
   the larger of both zones' clearances; keep-outs with `no_pours` on that layer.
3. Same-net pads: `solid` merges them; `thermal` keeps a `thermal_gap` around the pad (default: the clearance) and
   adds spokes of width `thermal_spoke` (default: max(net track width, 0.25 mm)) in the four axis directions;
   `none` keeps a clearance gap. Same-net tracks and vias are always solid.
4. Opening by `min_width / 2` removes copper narrower than `min_width` (default: rules `zone_min_width`).
5. Spokes are added only when they lie entirely in the allowed area of step 2 and reach the pour (a spoke that
   would violate a clearance is dropped, never trimmed into a sliver).
6. Islands not touching a pad, via or track of the zone's net are removed (netless zones keep all islands and
   treat every item as an obstacle).

Zone clearance defaults to max(rules clearance, the zone net's class clearance). Arcs are approximated with
5 µm tolerance, outward for obstacles and inward for fill boundaries, and every keep-away region carries a 10 nm
margin covering vertex rounding, so approximation never violates a clearance. Output is a canonical
`PolygonSet` per (zone, layer), in board order: `zones::fill_zones(project, base_items)`.

Fills join the shared geometry: `copper_items` = `base_copper_items` (pads, tracks, vias) + one
`ItemRef::Zone(id, layer, island)` item per fill island (`zone#12@F.Cu/0`). Connectivity, the ratsnest
(pads joined through a pour are connected; zone items are never ratsnest endpoints), DRC, rendering and Gerber
output see zone copper through it.

Performance and caching: layers fill in parallel (zones interact only within a layer), and later zones of a
layer reuse earlier fills grown by their keep-away distance. A 100 × 100 mm two-layer
board with ~550 copper items (120 pads, 200 tracks, 240 vias) fills both GND layers in ~110–140 ms (release);
large boards: see "Performance".
Fills are not stored on disk: `copper_items` has no project directory, and a recompute is cheap. Instead
`fill_zones` keeps the last 4 results in process, keyed by the exact input bytes (board, base copper, NPTH holes,
net classes; FNV-1a for lookup, full comparison for equality), so repeated queries on the same state in one MCP
or batch session cost ~1.5 ms.

## Commands

| Group | Commands |
|---|---|
| `board` | `setup` (layers, thickness, preferences), `outline` (rect, polygon, circle, rounded rect), `rules`, `info`, `ratsnest`, `sync` (add footprints for new components, drop removed ones) |
| `place` | `set` (at, rotation, side), `move` (relative), `rotate`, `flip`, `lock`, `list`, `auto` (initial grid by schematic groups), `near` (place a part next to another's pin) |
| `track` | `add` (polyline through points on a layer, width from net class/rules), `remove`, `list` |
| `via` | `add`, `remove` |
| `zone` | `add` (outline: points, `{"rect": {from, to}}` or `"board"`), `set`, `remove`, `list`, `fill` (report area/islands, warn empty or split) |
| `keepout` | `add` (forbid tracks, vias, pours, footprints; all when none given), `remove`, `list` |
| `drc` | `run` (M4 DRC workstream) |
| `render` | `board` (layers, realistic) (M4 rendering workstream) |
| `export` | `gerber`, `drill`, `pnp`, `ipc356`, `fab` (M4 outputs and fab workstreams) |

## DRC (`src/drc.rs`, `drc.run`)

`drc::check(project)` returns diagnostics sorted by code, then location; `drc.run` reports them (the CLI exits 3
on errors). Each has the objects involved (pins `U1.3`, `track#12`, `via#4`, `net:GND`, designators), a board
location and a fix hint. Clearance and track width come from the net's class when it sets them, else from
`board.rules`. Distances are exact between the shared polygon shapes; since arcs are approximated outward by up
to 1 µm, distance rules accept a 2 µm deficit.

| Code | Severity | Rule |
|---|---|---|
| `drc.no_outline` | error | the board has no outline (edge rules are skipped) |
| `drc.unplaced`, `drc.no_footprint` | warning | component not placed / placed without a footprint |
| `drc.short` | error | copper of different nets touches on a shared layer (also no-net copper touching a net) |
| `drc.clearance` | error | copper of different nets (or no net vs a net) closer than the larger of the two clearances; pads of one footprint are not checked against each other |
| `drc.track_width` / `drc.track_width_class` | error / warning | track narrower than `min_track_width` / than its net class width |
| `drc.via_drill`, `drc.pad_drill` | error | via or pad hole below `min_drill` |
| `drc.via_annular_ring`, `drc.pad_annular_ring` | error | (pad size − drill) / 2 below `min_annular_ring` (vias, plated pads) |
| `drc.hole_to_hole` | error | holes (vias, plated and non-plated pads) closer than `hole_to_hole`, edge to edge |
| `drc.outside_board` | error | copper not inside the outer contour, or overlapping a cutout |
| `drc.copper_to_edge` | error | copper closer than `copper_to_edge` to any contour |
| `drc.courtyard_overlap` | error | courtyards of two footprints on the same side overlap (touching is fine) |
| `drc.footprint_outside` | error | courtyard partly outside the board or over a cutout |
| `drc.silk_over_pad` | warning | footprint or board silkscreen closer than `silk_to_pad` to a pad on that side |
| `drc.keepout` | error | track, via or footprint courtyard inside a keep-out that forbids it (on its layers) |
| `drc.unrouted` | error | a ratsnest connection, with both ends |

Candidate pairs come from a uniform grid over bounding boxes (no extra dependency), so a board with a few
thousand items checks in well under a second in release builds (3000 tracks + 200 vias: about 75 ms). Zone
fills are tested through `board::prepared`, a convex outline is checked vertex by vertex (falling back to
`polyclip::contains`), and the ratsnest reuses the copper items (timings in "Performance").

## Performance

`tests/common/bigboard.rs` generates a deterministic synthetic board: 160 × 100 mm, four layers, 528
components (20 LQFP-100/LQFP-64/QFN-48 ICs, ~490 0402/0603 passives on both sides, 12 2 × 10 headers), 1399
nets, 2674 pads, 3040 tracks, 2434 vias, GND pour on In1.Cu and 3V3/1V8/5V pours on In2.Cu (routes cross, so
DRC has violations to report). `cargo run --release --example bigboard` times every heavy step (best of 3,
zone fill cache cleared before each run, i.e. what a fresh CLI process sees); `tests/perf.rs` has the same as an
`#[ignore]`d test plus equivalence checks against the previous algorithms. Apple Silicon laptop, release:

| Step | Before | After |
|---|---|---|
| `project.save` / `Project::load` | 16 / 7 ms | 17 / 7 ms |
| `placed_pads` | 46 ms | 4 ms |
| zone fill, 4 inner pours (`fill_zones_uncached`) | 2212 ms | 1371 ms |
| `islands` | 3309 ms | 39 ms |
| ratsnest from copper items (islands + MST) | 3484 ms | 40 ms |
| `ratsnest` (with fill) | 5612 ms | 1453 ms |
| `drc::check` (with fill) / fills cached | 8797 / ~6600 ms | 1587 / 206 ms |
| `render.board` PNG (with fill) / fills cached | 6331 / ~4100 ms | 1854 / 431 ms |
| `export.gerber` (with fill) / fills cached | 2505 / ~300 ms | 1569 / 167 ms |
| `board.export_kicad` | 62 ms | 27 ms |
| `render.schematic` (layout + PNG) | 1492 ms | 1538 ms |

What changed (outputs are byte-identical, D20): `placed_pads` builds one pin → net index instead of
scanning all nets per pin; `islands` skips pairs already connected and tests zone fills through
`board::prepared` (a segment grid and edge bands, so an item is tested against the pour's nearby edges only);
the ratsnest keeps each anchor's best link to the tree (O(k²) per net instead of rescanning every pair at
every Prim step); DRC computes copper items once, queries pours through `prepared` and checks a convex outline
vertex by vertex; rendering computes copper items once and unions copper layers in parallel; Gerber
coordinates are written without temporary strings; later zones reuse grown earlier fills; polygon booleans
use `polyclip`'s `rayon` feature (cadlab feature `parallel`, default on).

Remaining: zone fill is above the 1 s target. It is spent in `polyclip`'s `opening` and offsets of the large
fills (the GND fill has 360 k vertices and 1460 holes; one offset takes ~0.5 s, of which ~15 % is
re-normalizing already canonical input). Everything that needs the fill (cold ratsnest, DRC, render, Gerber)
inherits it; within one session the fill cache removes it. Speeding it up needs `polyclip` work (skip
normalization of canonical input, parallel offset of rings), not cadlab changes. Schematic rendering is
dominated by PNG encoding of the large sheet.

## Workstreams after the model lands

DRC, zone fill, board rendering, fab outputs (Gerber X2/X3, Excellon, IPC-D-356A, pick-and-place), fab profiles
(JLCPCB, PCBWay) with `fab check/compare/export`, and `.kicad_pcb` export with the KiCad DRC oracle are
independent of each other once the model and shared geometry exist, and are built in parallel.
