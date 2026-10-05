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
├── stackup       copper layer count and names, finished thickness, copper weights, dielectrics (optional:
│                 thickness, εr, material per gap, for impedance; docs/ELECTRICAL.md),
│                 board spec preferences (finish, mask/silk colors) — requirements, not a fab choice (D12)
├── outline       closed contours of line/arc segments: the first is the outer edge, others are cutouts
├── rules         design rules (engineering intent): clearance, track width, via drill/diameter, min annular
│                 ring, min drill, hole-to-hole, copper-to-edge, silk-to-pad, zone min width; net classes override
│                 per net (from the circuit)
├── footprints    by refdes: position, rotation, side, locked; footprint name defaults to the part's
├── tracks        segments (and arcs): layer, width, net, start, end, optional arc midpoint
├── vias          position, drill, diameter, net, layer span (through by default)
├── zones         polygon, layer(s), net, priority, clearance, min width, thermal relief settings
├── keepouts      polygon, layers, what is forbidden (tracks, vias, pours, footprints); forbidding
│                 nothing: a named rule area for custom rules
├── holes         mounting holes: name (H1), center, drill, plated pad diameter and net (none = NPTH)
├── graphics      silkscreen/fab/user drawings, filled polygons and texts; on copper layers they
│                 are netless copper
└── custom_rules  clearance / track width for a scope: item kinds, layer, footprint, courtyard,
                  rule area (see "Local settings, net ties and custom rules")
```

Footprints (in the project library, shared by their placements) carry the per-footprint and
per-pad settings of the next section: mask and paste margins, local clearance, zone connection,
net ties, pads on the back, mask openings per side, slotted holes, paste-in-hole, and copper,
mask and paste drawings.

## Shared geometry (`src/board/`)

One module turns the board into geometry that every consumer uses, so DRC, rendering, Gerber output and the
router agree on what copper exists:

- `pads(board, project)`: every placed pad with its absolute shape (polyclip polygon), layers, net, drill.
  Board holes are included as pads of designator = hole name (`H1.1` when plated, an unnumbered NPTH pad
  otherwise), so every consumer sees them (`src/board/holes.rs`).
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
2. Minus every other-net copper item on the layer (netless items too, copper drawings and texts included)
   inflated by max(zone clearance, the DRC clearance of the item and of the zone's net: the item's custom
   rule, else its local clearance, else its net's class clearance, else the rules'), so a zone clearance set
   below the rules (as KiCad zones often are) never makes the fill violate the DRC; NPTH holes (slots as
   stadiums) inflated by the zone clearance; higher-priority fills of other nets inflated by
   the larger of both zones' clearances; keep-outs with `no_pours` on that layer.
3. Same-net pads, by the pad's own zone connection (`footprint.set --zone-connection`, pad then
   footprint), else the zone's: `solid` merges them; `thermal` keeps a `thermal_gap` around the pad
   (default: the clearance) and adds spokes of width `thermal_spoke` (default: max(net track width,
   0.25 mm)) in the four axis directions; `none` keeps a clearance gap; `tht_thermal` is `thermal` for
   plated through-hole pads and `solid` for the others. Same-net tracks and vias are always solid.
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
layer reuse earlier fills grown by their keep-away distance. Each zone only receives what can reach its outline
(D42): the layer's items come from a packed R-tree (`geom::RTree`) queried with the outline's box grown by the
largest keep-away distance (each item's resolved clearance included), far NPTH holes and keep-outs are skipped, and earlier fills are grown only where
they can matter (polygons too far away dropped, holes too far away filled first). Same-net pad gaps and
thermal pads far from the area are skipped too, and rings of the pour meeting no thermal window are left out
of the spoke clips. Shapes that far away cannot change the fill, so the result is the same polygons (checked
against the previous algorithm, kept in `tests/common/fill_reference.rs`). A 100 × 100 mm two-layer
board with ~550 copper items (120 pads, 200 tracks, 240 vias) fills both GND layers in ~110–140 ms (release);
large boards: see "Performance".
Fills are not stored on disk: `copper_items` has no project directory, and a recompute is cheap. Instead
`fill_zones` keeps the last 4 results in process with their inputs (board, net classes and net assignments,
base copper with its local settings, NPTH holes and slots, and the clearance resolved for each base item, which
also depends on library footprints through custom rules and local clearances, D40) and reuses one only when every input compares equal (the board first by pointer: an
unchanged project shares it), so repeated queries on the same state in one MCP or batch session cost a few
milliseconds (5 ms on the synthetic board below, the NPTH list included).

## Local settings, net ties and custom rules (D40)

Footprints and pads carry settings that override the board's, all optional (unset: inherited), in
`Nm` (paste ratios as exact decimals), set with `footprint.set` (`pads` for pad settings, none for the
footprint's own defaults; `unset` resets). They belong to the land pattern, so every placement of the
footprint follows; nothing in them names a fab (D12).

| Setting | Where | Effect |
|---|---|---|
| `mask_margin` | pad, footprint; board `mask_expansion` | solder mask opening grown (shrunk) by it on every side, an exact offset (a rectangle grows with corners rounded by the margin); `export.gerber --mask-expansion` replaces the board's value for one export |
| `paste_margin`, `paste_ratio` | pad, footprint; board `paste_margin`, `paste_ratio` | paste opening resized by margin + ratio × side, per axis (a rounded rectangle keeps its corner ratio); exposed-pad paste windows and paste drawings are not resized |
| board `mask_min_web` | board | openings closer than it merge: the mask layer is written as the closing of all openings (grown by half the web, shrunk back, round joins), as regions |
| `clearance` | pad, footprint | see the clearance resolution below |
| `zone_connection` | pad, footprint | how zones of the pad's net join it (zone fill, step 3) |
| `back` | SMD pad | the pad's copper, mask and paste are on the other side of its footprint (card-edge fingers, heat sinks soldered from the back) |
| `mask` | pad | openings on the pad's outer copper (default), on the footprint's side only (`front`) or the other side only (`back`) for holes, or none (`none`: tented thermal vias, covered heat spreaders) |
| `slot` | through-hole and non-plated pads | an oval hole (size along the pad's X and Y); the drill is its width |
| `paste: pad` | through-hole pads | paste-in-hole on the footprint's side (SMD pads have paste unless `paste: none`) |
| `net_ties` | footprint | groups of pad numbers whose nets the footprint joins on purpose |
| copper, mask, paste drawings | footprint graphics (`back` for the other side) | copper: copper items (DRC, fills, Gerber, IPC-2581) on the net of the first pad they touch; mask, paste: openings on those layers |

**Clearance resolution** (`board::clearance`), for two copper items of different nets, highest first:
the last custom rule (board order) with a clearance whose scope holds either item; when either item
has a local clearance (pad, else footprint), the larger local value of the two, the other item's net
class not counting; else the larger net clearance (class, else `board.rules` `clearance`). Never below
`board.rules` `min_clearance` (0: no floor). Observed with `kicad-cli` on test boards, KiCad resolves
local overrides the same way (a pad's local 0.05 mm wins over a 0.3 mm net class of the other item,
the board minimum still applies), so imported boards check alike.

**Net ties.** Items of one net-tie group of one footprint (its pads, and its copper drawings, which
also bridge any copper of the group's nets) may touch: no `drc.short`, no `drc.clearance`, and
`board::islands` does not join them, so each net keeps its own connectivity (a net tie never routes
one net through another). Any other copper touching a tied pad is still a short, as in KiCad
(`shorting_items`).

**Custom rules** (`board.custom_rule`, `board.custom_rule_remove`, `board.custom_rules`) give a
clearance and/or a track width to the copper items in a scope: item `kinds` (pad, track, via, zone,
graphic), a copper `layer`, the pads and drawings of footprints matching a `footprint` pattern
(designator or footprint name, `*` `?`), items touching the courtyard of matching footprints
(`in_courtyard`), items entirely inside a named keep-out or rule area (`in_area`, `keepout.add
--rule-area`). A rule's clearance applies when either item is in scope (later rules win); its track
width replaces the net class width (`drc.track_width` when a track in scope is narrower; the board
minimum still applies). Zone fills keep each item's rule clearance too.

## Commands

| Group | Commands |
|---|---|
| `board` | `import_kicad` (a `.kicad_pcb` with its `.kicad_pro`/`.kicad_dru` rules, see "KiCad import"), `import_kicad_rules` (rules and net classes alone), `export_kicad`, `setup` (layers, thickness, preferences), `outline` (rect, polygon, circle, rounded rect), `rules` (show; `preset` ipc2/ipc3, `fab` + `process` + `margin` to derive from a fab profile, then field by field, also `min_clearance`, `mask_expansion`, `mask_min_web`, `paste_margin`, `paste_ratio`; see "Design rules"), `custom_rule` / `custom_rule_remove` / `custom_rules` (scoped clearance and track width, see "Local settings, net ties and custom rules"), `info`, `ratsnest`, `hole` (mounting hole: drill, optional plated `pad` and `net`, name H1...), `hole_remove`, `cutout` (rect, circle or polygon inside the outline), `cutout_remove` (by number), `sync` (add footprints for new components, drop removed ones) |
| `place` | `set` (at, rotation, side), `move` (relative), `rotate`, `flip`, `lock`, `remove`, `list`, `auto` (strategy `groups` (default) or `rows`; `spacing`, `replace`), `near` (next to a pin `U1.VDD` or a part; `side`, `distance`), `align` (X or Y of origins to first/center/min/max/value), `distribute` (equal or given gaps between courtyards) |
| `track` | `add` (polyline through points on a layer, width from net class/rules), `remove`, `list` |
| `via` | `add`, `remove` |
| `zone` | `add` (outline: points, `{"rect": {from, to}}` or `"board"`), `set`, `remove`, `list`, `fill` (report area/islands, warn empty or split) |
| `keepout` | `add` (forbid tracks, vias, pours, footprints; all when none given; `rule_area`: forbid nothing, a named area for custom rules), `remove`, `list` |
| `drc` | `run` (plus `fab.check` warnings for the manifest `targets`) |
| `board` (stackup) | `stackup` (copper, dielectrics, line model per layer), `dielectric` (thickness, εr, material per gap; [ELECTRICAL.md](ELECTRICAL.md)) |
| `impedance` | `calc` (Z0 / Zdiff of a width on a layer), `solve` (width for a target, optionally into a net class) |
| `current` | `width` (IPC-2152 width per layer for a current; optionally raises a net class width) |
| `render` | `board` (layers, realistic) (M4 rendering workstream) |
| `export` | `gerber`, `drill`, `pnp`, `ipc356`, `all` (generic outputs), `dsn` (Specctra design for external routers) |
| `fab` | `list`, `show`, `check`, `compare`, `export`, `substitute` (fab profiles, [MANUFACTURING.md](MANUFACTURING.md)) |
| `netclass` | `set`, `list`, `show` (own values, values in effect with those inherited from `board.rules`, nets), `remove` (circuit level, [DATA_MODEL.md](DATA_MODEL.md)) |

## Design rules

`board.rules` holds engineering intent: numbers only, never a reference to a preset or a fab (D12, D26). It
is built in this order, each step overriding the previous one: the current rules or a `preset`, then values
derived from a fab profile (`fab`, `process`, `margin`), then the fields given explicitly. The output lists
what changed and, for derived values, the profile field and the fab's own limit each comes from; a value the
profile marks unverified gives a `board.rules_unverified` warning. Net classes whose values fall below the new
minimums are reported (`drc.netclass_rule`). `--dry-run` previews a change.

Net classes (`netclass.*`) override per net: track width, clearance, via drill and diameter, diff pair width and
gap. Unset values are inherited from `board.rules`; `netclass.show` / `netclass.list` give the values in
effect (`effective`, inherited fields listed). The DRC uses the class clearance and width for the class's
nets, warns about tracks and vias smaller than their class asks (`drc.track_width_class`,
`drc.via_size_class`) and about class values below the board minimums (`drc.netclass_rule`).

### Rule presets (`board.rules --preset`)

| Field | `ipc2` (= a new project) | `ipc3` |
|---|---|---|
| clearance | 0.20 mm | 0.20 mm |
| track width / minimum | 0.25 / 0.15 mm | 0.25 / 0.15 mm |
| via drill / diameter | 0.30 / 0.60 mm | 0.30 / 0.80 mm |
| minimum annular ring | 0.13 mm | 0.25 mm |
| minimum drill | 0.30 mm | 0.30 mm |
| hole to hole, copper to edge | 0.50, 0.30 mm | 0.50, 0.30 mm |
| silk to pad, silk width, zone minimum width | 0.15, 0.15, 0.20 mm | 0.15, 0.15, 0.20 mm |
| `ipc_class` | 2 | 3 |

Sources and status. The IPC standards are paywalled and were **not read**; the numbers below come from
secondary sources (fab and EDA vendor articles quoting the standards, checked 2026-10-04) and are marked
**unverified** until checked against the documents themselves:

- IPC-6012 minimum external annular ring of the finished board: class 2 allows 90° breakout; class 3 requires
  0.050 mm (internal layers 0.025 mm), no breakout. *Unverified* (e.g.
  <https://resources.altium.com/p/meeting-standards-ipc-6012-class-3-annular-ring>,
  <https://www.protoexpress.com/kb/ipc-class-3-pcb-design-and-manufacturing-standards/>).
- IPC-2221 land size: land = finished hole + 2 × (minimum annular ring) + fabrication allowance, the allowance
  being 0.6 / 0.5 / 0.4 mm for producibility levels A / B / C. *Unverified*
  (<https://resources.altium.com/p/pcb-size-and-pad-size-guidelines>, pcblibraries.com forum).
- IPC-2221 Table 6-1 conductor spacing up to 15 V: 0.05 mm internal (B1), 0.1 mm external uncoated (B2),
  0.13 mm external with permanent polymer coating (A6). *Unverified*.

How the presets use them: `ipc3`'s annular ring is the class 3 external minimum plus half the level C
allowance (0.05 + 0.4 / 2 = 0.25 mm), with default vias sized to give that ring (0.3 / 0.8 mm). Everything
else in both presets is cadlab's own conservative choice, not an IPC number: the 0.20 mm clearance is above
the Table 6-1 low-voltage values (higher voltages need more, M8 adds a calculator), and `ipc2`'s 0.13 mm ring
leaves margin over the breakout allowance that mainstream fabs' published via capabilities (0.05 to 0.15 mm)
show is enough. Fab-specific limits always come from fab profiles, not from these presets.

### Rules from a fab profile (`board.rules --fab <id> [--process P] [--margin M]`)

| Rule | Profile field | `tightest` | `comfortable` (default) |
|---|---|---|---|
| `min_track_width`, `clearance`, `min_drill`, `min_annular_ring` | `min_track`, `min_space`, `min_drill`, `min_via_ring` | the fab's limit | limit × 1.25, rounded up to 10 µm |
| `hole_to_hole`, `copper_to_edge`, `silk_to_pad`, `min_silk_width` | same names | the fab's limit | limit × 1.25, rounded up to 10 µm |
| `track_width`, `zone_min_width` | `min_track` | = `min_track_width` | max(`min_track_width`, 0.25 / 0.20 mm) |
| `via_drill` | `min_drill` | = `min_drill` | max(`min_drill`, 0.30 mm) |
| `via_diameter` | `min_via_ring` | drill + 2 × ring | max(drill + 2 × ring, 0.60 mm) |

Fields the profile leaves out keep their value; `ipc_class` is kept. The pad annular ring (`min_pth_ring`) and
the pad hole-to-hole distance stay `fab.check` checks: cadlab's rules have one annular ring for vias and pads.
For JLCPCB's two-layer process, `tightest` gives 0.10 mm track/space and 0.15 / 0.25 mm vias, `comfortable`
0.13 mm track/space minimums with 0.25 mm tracks and 0.3 / 0.6 mm vias.

## KiCad import (`src/kicad_import/`, `board.import_kicad`)

Migrates a KiCad board into a project (D32). Written from KiCad's published file format documentation and files
`kicad-cli` writes; KiCad 6 to 10 formats are read (nets by number and net table, or by name as KiCad 10 writes
them). Older files are refused with a hint to run `kicad-cli pcb upgrade`. Without `replace` the board must be
empty; with it every placement, copper item, area, hole, drawing and the outline are replaced (the circuit and
the library stay).

| KiCad | cadlab |
|---|---|
| layer table, `general` thickness, stackup copper thicknesses, finish, mask/silk colors | `stackup` (dielectric layers are not kept: info) |
| auxiliary axis origin, else the Edge.Cuts lower-left corner (`origin`: `auto`, `outline`, `aux`, `page`) | cadlab's (0, 0); Y flipped |
| Edge.Cuts lines, arcs, circles, rectangles, polygons, curves (also inside footprints) | `outline`: chained into closed contours (ends up to 10 µm apart joined), the largest is the outer edge, contours inside it cutouts; open chains and contours outside are reported |
| embedded footprint | a project footprint (shared by identical instances, reusing a project footprint equal within 3 nm) and a placement: position, rotation, side, lock |
| pads: rect, circle, oval, roundrect | the same shapes (rounded corner = ratio × shorter side) |
| pads: custom | polygon pads: anchor ∪ primitives (filled polygons, stroked lines and arcs, circles, rectangles; curves within 1 µm outside), holes joined by zero-width cuts; only the largest part of a pad in separate parts (`import.pad_approximated`) |
| pads: trapezoid, chamfered | bounding rectangle, rounded corners (`import.pad_approximated`) |
| oval drill (slot) | a slotted hole (`slot`); a drill offset is ignored, the hole centered (`import.pad_approximated`) |
| SMD pad with copper only on the other side of its footprint | a pad on the back (`back`) |
| pad layers without `*.Mask` / `F.Mask` / `B.Mask` | the pad's `mask`: none (tented), front or back only |
| through-hole pad with `F.Paste` | paste-in-hole (`paste: pad`; paste on the other side only: `import.tht_paste`) |
| paste-only apertures | paste windows of the copper pad they sit on (equal aligned rectangles); others (custom shapes, mixed sizes, off a pad) become paste drawings of the footprint (`import.paste_drawing`) |
| mask-only apertures | mask drawings of the footprint |
| copper, mask and paste drawings of either side (`fp_line`, `fp_poly`, ...) | footprint graphics on those layers (`back` for the other side); filled polygons and rectangles are areas, outlines closed polylines; inner-layer copper is not imported (`import.footprint_layer`) |
| silkscreen, fab and courtyard drawings of the footprint's side | footprint graphics (arcs as polylines); the courtyard replaces the courtyard drawings: one closed polygon as is, a circle as a polygon around it, lines forming one loop, several closed shapes as their union when it is one polygon, else the bounding box; none: 0.25 mm around the pads (`import.courtyard_generated`, listing every footprint) |
| `MountingHole*` footprints / `H`, `MH` designators with one round hole pad | board holes (plated with net, or NPTH) |
| board-only footprints (`board_only`, not in the circuit, or a designator cadlab cannot use) made of round holes | one plated hole on a net (stitching via footprints): a through via; otherwise one board hole per pad (`import.footprint_as_via`, `import.footprint_as_holes`) |
| `solder_mask_margin`, `solder_paste_margin`, `solder_paste_margin_ratio` / `solder_paste_ratio`, `clearance`, `zone_connect` (pad, footprint) | the pad's or footprint's settings (zero margins and clearances are KiCad's "not set" and stay unset; `zone_connect` 0 none, 1 thermal, 2 solid, 3 `tht_thermal`) |
| setup `pad_to_mask_clearance`, `solder_mask_min_width`, `pad_to_paste_clearance`, `pad_to_paste_clearance_ratio` | `board.rules` `mask_expansion`, `mask_min_web`, `paste_margin`, `paste_ratio` |
| `net_tie_pad_groups` (`"1, 2"`) | the footprint's `net_ties` |
| project text variables (`${NAME}` in board texts) | replaced by their `.kicad_pro` values |
| footprints without pads (logos) | board graphics (copper, mask and paste polygons stay filled areas) |
| `segment`, `arc`, `via` (through, blind, micro) | tracks (arcs keep their mid point), vias |
| copper `zone` | zone: outline, net, layers, priority, clearance, minimum width, pad connection (`thru_hole_only`: `tht_thermal`), thermal gap and spoke; fills recomputed; values equal to cadlab's defaults left unset |
| rule area (`keepout`) | keep-out: tracks, vias, copper pour, footprints (all copper layers = no layer list); one that forbids nothing is kept as a rule area when a custom rule names it (`enclosedByArea`), else reported (`import.rule_area`) |
| zone or rule area with several `polygon` outlines | their area under the even-odd rule (a polygon inside another is a hole): one zone or keep-out per separate part, holes joined to the outline by zero-width cuts (`import.zone_outlines`) |
| `gr_line`/`gr_arc`/`gr_circle`/`gr_rect`/`gr_poly`, `gr_text` | board graphics (lines, arcs and circles as polylines; filled polygons, rectangles and circles as areas; texts with size and angle); on copper layers they are netless copper (a net they carry is reported, `import.copper_drawing_net`) |

Nets: with a circuit in the project, footprints are matched by designator, pads to pins through the part's
pin map, and each board net takes the circuit name most of its pads carry (`import.net_conflict`,
`import.net_mismatch` when they disagree); footprints the circuit lacks are not placed
(`import.component_not_in_circuit`); a part without a footprint gets the board's. Without a circuit, it is built
from the footprints through the netlist importer (D27): designators, values, fields (MPN, manufacturer),
`pinfunction`/`pintype` as pin names and types, one pin per pad number, generic passives where the footprint
gives a size. Net names lose KiCad's root sheet `/`; single-pad `unconnected-(...)` nets become no net. A
footprint differing from the part's preferred one is placed through the placement's footprint override.

Rules (`.kicad_pro`, `.kicad_dru` next to the board, or `board.import_kicad_rules`): board minimums and the
Default class give `board.rules` (clearance = the larger of `min_clearance` and the Default class clearance),
other classes become net classes (values equal to the board's are inherited), patterns and explicit
assignments give nets their class, custom rules without condition tighten board minimums and `A.NetClass ==
'X'` width and clearance rules tighten the class. Rules with geometric conditions become custom rules
(`board.custom_rules`) when every term of the condition (joined by `&&`) is one of `A.Type == 'Pad'`
(`Track`, `Via`, `Zone`, `Graphic`; `!=` too), `A.Layer == '<copper layer>'`, `A.memberOfFootprint('…')`,
`A.intersectsCourtyard('…')` (the library prefix of a footprint pattern is dropped: cadlab footprint
names have none) or `A.enclosedByArea('…')`, or a parenthesized `||` of one kind of term (as the export
writes them); a rule `(layer ...)` naming one copper layer is the layer term; their `clearance` and
`track_width` minimums are kept (a maximum is not checked: `import.rule_partial`). Not supported, each
reported (`import.rule_unsupported`): conditions on `B`, other functions (`hasComponentClass`,
`insideArea`, ...), net class terms mixed with geometric ones, other constraints in scoped rules
(`disallow`, `courtyard_clearance`, `silk_clearance`, ...), and rules switched off
(`severity ignore` or `exclusion`). Zero minimums keep cadlab's value (`import.rule_zero`), except `min_silk_clearance`, which
becomes `silk_to_pad` even at zero (silkscreen may then touch pads but not cover them, as KiCad checks
it).

Never silent: every item not imported is a warning (code, subject, hint) counted in `not_imported`; repeated
notes are aggregated with a count. Not supported: dimensions, images, text boxes, tables, targets, groups
(members are imported), footprint texts, 3D models, copper drawings on inner layers of footprints, zones
inside footprints, per-layer pad stacks (the front layer's shape is kept, `import.pad_approximated`),
drill offsets, hatched fills (solid), teardrop zone attributes (teardrops are ordinary zones). The open-source corpus (docs/TESTING.md) measures what these gaps cost on real boards. The library returns KiCad UUID → imported object labels (`BoardImportReport::uuids`) so KiCad reports
can be read against the imported project.

## Placement (`src/board/place.rs`)

Courtyards are handled as their boxes at quarter-turn rotations. The allowed area of a side is the outer contour
shrunk by max(`copper_to_edge`, spacing), minus cutouts grown by that margin, `no_footprints` keep-outs, holes and
the courtyards of footprints that stay (locked ones, and placed ones unless `replace`), grown by the spacing.

`place.auto` with strategy `groups` (default):

1. Grouping, as in the schematic layout: anchors are ICs (more than two pins) and connectors (part category);
   a decoupling capacitor (one pad on ground, one on a power net) goes to a power pin of an anchor on its rail
   (inputs first, then regulator outputs, spreading capacitors over pins); other passives go to the anchor
   sharing a signal net (ICs before connectors), then chain through signal nets (R then LED), then to an anchor
   sharing a power net.
2. Anchors, most connected to what is already placed first, on a coarse grid (about 4000 positions, at least
   0.5 mm) with all rotations: the first IC near the center, other ICs pulled to their connections, connectors
   to the nearest edge with their long side along it. Each anchor keeps a margin around it sized from its
   group's area, so its passives fit.
3. Passives, group by group (decoupling capacitors first, then chains), on a 0.25 mm grid within 3 mm, then
   8 mm, of their target pin, all rotations: the score is the distance from their connecting pad to the
   target pin (weighted 4 for decoupling, 2 otherwise) plus the distance of their other pads to the nearest
   placed pin of the same net (ground weighted 0.3). Candidates are tried best first; the first valid one wins.
4. Improvement: up to 12 passes of greedy moves per part (shifts of 0.25 to 4 mm in 8 directions, rotations)
   and swaps of identical footprints, accepting the best valid move that lowers the ratsnest (MST per net over
   pad centers) plus tethers keeping passives at their target pins; connectors never move away from the edge.

On the ATtiny85 test board (10 parts, 40 × 30 mm) this gives a ratsnest of about 97 mm against 175 mm for
`rows`, the 3V3 decoupling capacitor pad 1.6 mm from the MCU's VCC pad (C1 1.75 mm from the LDO input), no DRC
placement errors.

`place.near` puts the part just outside the target's courtyard (on `side`, default the side the pin faces),
`distance` between courtyards (default 0.25 mm), with its pad of the target's net aligned with the pin; each
rotation is tried and the part slides along the side, then away from it, until it fits; the rotation with the
shortest link wins.

## DRC (`src/drc.rs`, `drc.run`)

`drc::check(project)` returns diagnostics sorted by code, then location; `drc.run` reports them (the CLI exits 3
on errors). Each has the objects involved (pins `U1.3`, `track#12`, `via#4`, `net:GND`, designators), a board
location and a fix hint. Clearance and track width come from the net's class when it sets them, else from
`board.rules`. Distances are exact between the shared polygon shapes; since arcs are approximated outward by up
to 1 µm, distance rules accept a 2 µm deficit, and area rules (courtyard overlaps, keep-outs, holes in
courtyards) count an overlap only where it is wider than 2 µm.

| Code | Severity | Rule |
|---|---|---|
| `drc.no_outline` | error | the board has no outline (edge rules are skipped) |
| `drc.unplaced`, `drc.no_footprint` | warning | component not placed / placed without a footprint |
| `drc.short` | error | copper of different nets touches on a shared layer (also no-net copper touching a net); copper of one net-tie group of a footprint may touch |
| `drc.clearance` | error | copper of different nets (or no net vs a net) closer than the clearance in effect (custom rule, local clearances, else the larger net clearance; see "Local settings, net ties and custom rules"); pads and copper drawings of one footprint are not checked against each other |
| `drc.track_width` / `drc.track_width_class` | error / warning | track narrower than `min_track_width` or a custom rule's width / than its net class width |
| `drc.via_size_class` | warning | via drill or diameter smaller than its net class asks |
| `drc.netclass_rule` | warning | a net class value below the board minimums (track or diff pair width < `min_track_width`, via drill < `min_drill`, via ring < `min_annular_ring`) |
| `drc.via_drill`, `drc.pad_drill` | error | via or pad hole below `min_drill` |
| `drc.via_annular_ring`, `drc.pad_annular_ring` | error | (pad size − hole) / 2 below `min_annular_ring` (vias, plated pads; slots per pad axis) |
| `drc.hole_to_hole` | error | holes (vias, plated and non-plated pads, slots as stadiums) closer than `hole_to_hole`, edge to edge; holes of one pin (pads of a footprint sharing a number) are not checked against each other |
| `drc.outside_board` | error | copper or a non-plated board hole not inside the outer contour, or overlapping a cutout |
| `drc.copper_to_edge` | error | copper closer than `copper_to_edge` to any contour |
| `drc.courtyard_overlap` | error | courtyards of two footprints on the same side overlap (touching is fine), or a board hole is inside a courtyard (either side) |
| `drc.footprint_outside` | error | courtyard partly outside the board or over a cutout |
| `drc.silk_over_pad` | warning | footprint or board silkscreen closer than `silk_to_pad` to a pad with a mask opening on that side |
| `drc.keepout` | error | track, via or footprint courtyard inside a keep-out that forbids it (on its layers) |
| `drc.unrouted` | error | a ratsnest connection, with both ends |
| `drc.current_width` | warning | tracks of a net with a `current` narrower than IPC-2152 asks on their layer ([ELECTRICAL.md](ELECTRICAL.md)) |
| `drc.impedance` | warning | tracks of a net whose class has an `impedance` target more than 10 % off on their layer |

Candidate pairs come from a uniform grid over bounding boxes (no extra dependency), so a board with a few
thousand items checks in well under a second in release builds (3000 tracks + 200 vias: about 75 ms). Zone
fills are tested through `board::prepared`, a convex outline is checked vertex by vertex, and the ratsnest
reuses the copper items (timings in "Performance"). Other outlines have their segments in R-trees: an item no
outline segment comes near is inside exactly when one of its points is (even-odd parity, as `polyclip`
locates), an item no edge segment comes within `copper_to_edge` of passes the edge rule, and only the rest
asks `polyclip::contains` / `distance_less_than` on the whole contour (D42). The pair grid stays: on the corpus
it lists candidate pairs in 1.4–3.9 ms where an R-tree self-join takes 3.3–5.7 ms.

## Performance

`tests/common/bigboard.rs` generates a deterministic synthetic board: 160 × 100 mm, four layers, 528
components (20 LQFP-100/LQFP-64/QFN-48 ICs, ~490 0402/0603 passives on both sides, 12 2 × 10 headers), 1399
nets, 2674 pads, 3040 tracks, 2434 vias, GND pour on In1.Cu and 3V3/1V8/5V pours on In2.Cu (routes cross, so
DRC has violations to report). `cargo run --release --example bigboard` times every heavy step (best of 3,
zone fill cache cleared before each run, i.e. what a fresh CLI process sees); `tests/perf.rs` has the same as an
`#[ignore]`d test plus equivalence checks against the previous algorithms. Apple Silicon laptop, release
(the D42 column was measured on a loaded machine, so small differences are noise; the last column is
`polyclip` 0.0.6, outputs byte-identical):

| Step | Before D23 | After D23 | After D42 | polyclip 0.0.6 |
|---|---|---|---|---|
| `project.save` / `Project::load` | 16 / 7 ms | 17 / 7 ms | 17 / 7 ms | 16 / 7 ms |
| `placed_pads` | 46 ms | 4 ms | 4 ms | 4 ms |
| zone fill, 4 inner pours (`fill_zones_uncached`) | 2212 ms | 1371 ms | 1000 ms | 147 ms |
| `islands` | 3309 ms | 39 ms | 42 ms | 41 ms |
| ratsnest from copper items (islands + MST) | 3484 ms | 40 ms | 43 ms | 42 ms |
| `ratsnest` (with fill) | 5612 ms | 1453 ms | 1059 ms | 203 ms |
| `drc::check` (with fill) / fills cached | 8797 / ~6600 ms | 1587 / 206 ms | 1170 / 199 ms | 317 / 175 ms |
| `render.board` PNG (with fill) / fills cached | 6331 / ~4100 ms | 1854 / 431 ms | 1363 / 429 ms | 392 / 251 ms |
| `export.gerber` (with fill) / fills cached | 2505 / ~300 ms | 1569 / 167 ms | 1131 / 159 ms | 308 / 146 ms |
| `board.export_kicad` | 62 ms | 27 ms | 30 ms | 31 ms |
| `render.schematic` (layout + PNG) | 1492 ms | 1538 ms | (unchanged code) | |
| `place.auto` (all 528 parts, `replace`) | | 182 s | 39 s | |

What changed in D23 (outputs are byte-identical, D20): `placed_pads` builds one pin → net index instead of
scanning all nets per pin; `islands` skips pairs already connected and tests zone fills through
`board::prepared` (a segment grid and edge bands, so an item is tested against the pour's nearby edges only);
the ratsnest keeps each anchor's best link to the tree (O(k²) per net instead of rescanning every pair at
every Prim step); DRC computes copper items once, queries pours through `prepared` and checks a convex outline
vertex by vertex; rendering computes copper items once and unions copper layers in parallel; Gerber
coordinates are written without temporary strings; later zones reuse grown earlier fills; polygon booleans
use `polyclip`'s `rayon` feature (cadlab feature `parallel`, default on).

What changed in D42 (outputs byte-identical again): a packed R-tree (`geom::RTree`); zone fill gives each zone
only what can reach its outline (see "Zone fill"); the fill cache compares its inputs instead of serializing
them (cached lookup 15 → 5 ms); the DRC indexes the outline's segments for the edge and containment rules; PNG
rendering skips what leaves no pixel (a cropped `--around` view: raster 48 → 28 ms); `place.auto` computes each
net's MST in one pass per step and reuses net costs across swap candidates (it spent 170 of 182 s there, under
1 s in validity checks); `polyclip` 0.0.4. The router keeps its bucket grid: its index changes while routing
(an R-tree packed once does not fit) and queries are under 10 % of `route.all` on the small boards.

`cargo run --release --example bigboard corpus` times the open-source corpus boards (`CADLAB_CORPUS_DIR`,
docs/TESTING.md), cold (fill cache cleared), before → after D42 → with `polyclip` 0.0.6:

| Board | Zones | Zone fill | `drc::check` | `render.board` | `export.gerber` |
|---|---|---|---|---|---|
| corne-cherry | 925 (teardrops) | 4238 → 613 → 295 ms | 4724 → 758 → 420 ms | 4362 → 726 → 379 ms | 4225 → 663 → 380 ms |
| cynthion | 57 | 568 → 429 → 227 ms | 1232 → 653 → 416 ms | 964 → 737 → 488 ms | 714 → 556 → 368 ms |
| glasgow-revD1 | 7 | 685 → 711 → 349 ms | 1048 → 1053 → 620 ms | 1099 → 1087 → 662 ms | 976 → 945 → 587 ms |
| lumenpnp-mobo | 82 | 599 → 578 → 309 ms | 848 → 816 → 533 ms | 874 → 842 → 510 ms | 703 → 667 → 404 ms |
| sweep-v2.2 | 2 | 144 → 146 → 76 ms | 260 → 204 → 138 ms | 212 → 213 → 159 ms | 161 → 163 → 112 ms |
| buspirate5-rev10 | 15 | 237 → 252 → 138 ms | 334 → 351 → 215 ms | 413 → 432 → 269 ms | 315 → 333 → 241 ms |
| glasgow-revC3 | 31 | 196 → 199 → 44 ms | 428 → 422 → 194 ms | 410 → 403 → 196 ms | 308 → 296 → 122 ms |
| tinytapeout-demo | 3 | 281 → 262 → 159 ms | 436 → 413 → 284 ms | 507 → 488 → 330 ms | 356 → 337 → 225 ms |

`polyclip` 0.0.5–0.0.6 implemented the wishlist in [POLYGON_LIB.md](POLYGON_LIB.md) (canonical input is no longer re-normalized, fused
opening, booleans cluster by cluster, parallel noding and sweep within a cluster): the synthetic fill went from
1.0 s to ~150 ms and the corpus boards' cold DRC, render and Gerber 2–3× faster, outputs byte-identical. One
cadlab change came with it: obstacle shapes are concatenations of item outlines that may overlap (a teardrop
over its pad), and an overlapping set can pass `polyclip`'s cheap "looks canonical" test, in which case
`offset` grows the rings without merging them first, valid output that rounds a few nanometers differently
(5 of corne-cherry's 925 teardrop fills changed). The fill now unions each obstacle group before growing it,
so `offset` always gets canonical input and fills do not depend on that heuristic. Everything that needs the
fill (cold ratsnest, DRC, render, Gerber) inherits its cost; within one session the fill cache removes it.
Schematic rendering is dominated by PNG encoding of the large sheet.

## Workstreams after the model lands

DRC, zone fill, board rendering, fab outputs (Gerber X2/X3, Excellon, IPC-D-356A, pick-and-place), fab profiles
(JLCPCB, PCBWay) with `fab check/compare/export`, and `.kicad_pcb` export with the KiCad DRC oracle are
independent of each other once the model and shared geometry exist, and are built in parallel.
