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
  already connects), from the circuit's nets.
- `connectivity(board, project)`: copper islands and which nets they join (shorts) or split (opens).

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

Performance and caching: layers fill in parallel (zones interact only within a layer). A 100 × 100 mm two-layer
board with ~550 copper items (120 pads, 200 tracks, 240 vias) fills both GND layers in ~110–140 ms (release).
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

## Workstreams after the model lands

DRC, zone fill, board rendering, fab outputs (Gerber X2/X3, Excellon, IPC-D-356A, pick-and-place), fab profiles
(JLCPCB, PCBWay) with `fab check/compare/export`, and `.kicad_pcb` export with the KiCad DRC oracle are
independent of each other once the model and shared geometry exist, and are built in parallel.
