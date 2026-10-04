# Rendering

cadlab has no UI. Rendering lets humans review a design, and lets multimodal agents check their own work.

## Pipeline

```
model ──► Scene (lines, polygons, circles; text as strokes) ──► SVG writer (mm units, exact)
                                                           └──► PNG rasterizer (tiny-skia)
```

- One scene builder per view (`schematic::draw`, footprint view, later board views). SVG and PNG are drawn from
  the same primitives, so they match; neither needs fonts installed.
- Text uses the Hershey Simplex stroke font, embedded (`src/render/font/`, see its `NOTICE`), turned into
  polylines when added to the scene.
- PNG resolution is set in pixels per millimeter (`--px-per-mm`, default 10 for sheets, 80 for footprints; boards
  default to about 1600 px on the longer side).
- PNG support is the default `png` feature (tiny-skia); SVG has no extra dependency.
- Over MCP, a rendered PNG is returned as image content in the tool result.

## Views

| View | Contents |
|---|---|
| `schematic` | auto-laid-out symbols, wires, junctions, labels, power symbols, refdes/values, block frames; one or more sheets (`render.schematic`, see below) |
| `symbol` | one part's symbol (`render.symbol`) |
| `footprint` | pads, paste windows, courtyard, silkscreen, fab outline (`render.footprint`) |
| `board` | selected layers (`--layers`, default all copper + silk + outline) in viewer colors on a dark background: copper semi-transparent and unioned per layer (front red, back blue, inner amber/green/purple/...), silk white (front) / yellow (back), fab grey, courtyard magenta, mask/paste openings, outline yellow, drill holes; bottom-side footprints mirrored on `B.*` layers; refdes at the footprint origin on its silk layer, sized to the courtyard (`render.board`, `src/render/board.rs`) |
| `board --around U1` | the same, cropped to a component's courtyard plus `--margin` (default 3 mm) |
| `board --realistic top\|bottom` | solder mask over laminate and copper, silkscreen, exposed pads in the finish color (ENIG gold, HASL silver, OSP copper), colors from the `board.setup` preferences; the bottom view is mirrored as seen from below |
| `board3d` | isometric / any-angle 3D view of the assembled board with component bodies (`render.board3d`, see "3D view") |
| `ratsnest` overlay | unrouted connections as thin straight lines (on by default in `render.board`, `--ratsnest false` hides) |
| `drc` overlay | markers + labels at given points (`render.board --markers '[{"at": ["5mm", "3mm"], "label": "clearance"}]'`) |
| `highlight` | emphasize nets/components (`render.board --highlight GND,U1`), dim everything else |
| `placement` | courtyards, refdes, orientation markers only: fast layout review |

Annotations (dimensions, grid, scale bar, legend) are optional so agents can read real distances from images.

## Schematic layout

The schematic is derived from the circuit (`src/schematic/layout.rs`), on the 2.54 mm grid with pins also on
KiCad's grid (Y measured from the top edge). It is built from groups, each laid out on its own:

- **Anchor groups**: every box symbol (IC, connector) with what attaches to its pins:
  - series parts inline on the pin, chains continuing outward through two-pin nets (`PB3 → R2 → D1 → GND`);
  - parts between the pin and a supply (pull-ups, pull-downs, filter capacitors, buttons) as branches hanging off
    the pin's wire (up to a supply, down to ground), or inline when that is more compact;
  - a crystal between two pins of one side, next to them, with its load capacitors to ground (the symbol
    generator keeps `OSC*`/`XTAL*` pins together on one side);
  - decoupling capacitors (both pins on supplies) on shared supply and ground wires under their IC; bulk
    capacitors (≥ 1 µF) go to the regulator driving their supply;
  - consecutive pins of one supply net on a side share one power symbol on a short bus;
  - a wired net that continues elsewhere is named on its wire; every other pin gets a net label or power symbol.
- **Chains**: two-terminal parts left over, as vertical chains (supply or signal on top, ground at the bottom).
- **Block instances**: the groups of a block instance's components (`Component.block`), packed together in a
  frame titled `instance (block)`.

Each element is placed only where it collides with nothing already drawn: boxes for symbol bodies and pin
strips, designators and values (from the stroke font's text extents, with a small clearance), labels and power
symbols; wires must not cross boxes or other wires, nor touch a connection point of another net. Attachments
try several distances, crystal positions and option sets, keeping the one that places the most parts; a pin
whose parts do not fit keeps its net label. `schematic::overlaps` checks a finished sheet with the same geometry
(`tests/schematic_layout.rs` runs it on the ATtiny85, STM32 and multi-sheet boards).

Groups are packed with a skyline bin packer (several orders, fewest sheets, then fewest split groups, then least
height) around the title block, onto the smallest paper that holds them:

- `render.schematic`: one A4 or A3 sheet when everything fits, otherwise as many A3 sheets as needed, written as
  `name-1.png`, `name-2.png`, ... (`sheet: N` renders one); the title block shows `sheet N/M`; groups are never
  split and the main circuit's groups stay together when possible;
- `schematic.export` (KiCad): one sheet, A4 to A0 or a custom size (DECISIONS D24); frames become dashed
  rectangles with their title.

## 3D view (M9)

`render.board3d` (`src/render/board3d/`) draws the assembled board as an orthographic 3D view, isometric by
default. It is a small software renderer: no GPU, no 3D dependency (tiny-skia only rasterizes the face textures and
encodes the PNG).

- **Board**: the outline extruded to the stackup thickness, with walls for the outer edge, cutouts and every drill
  hole (plated walls in the finish color, others in laminate color). The top and bottom faces are textured with the
  realistic 2D render of that side (mask, exposed copper finish, silkscreen; `board --realistic`), rendered at about
  1.5 times the output resolution; the texture's coverage (outline minus holes, anti-aliased) cuts the face, so holes
  and cutouts are see-through. Only the visible face's texture is rendered.
- **Components**: convex solids built from the footprint's generator spec (`PackageSpec`): chips (body + tinned
  terminations, colored by kind), SOIC/SOP/SOT gull-wing and QFP leads (shoulder, leg, foot, toe at the lead span),
  DFN/QFN terminals showing at the body edges, SOT-223/DPAK tabs, SOD, SMA/SMB molded bodies, MELF cylinders, DIP
  (leads through the board), BGA (substrate + mold cap), pin headers (plastic body, gold pins 6 mm above the body and
  3 mm below the board). Leads sit where the footprint's pads are, so missing pins follow the land pattern. Pin 1 is a
  light dot on ICs (on pad 1's side), polarized two-terminal parts get a cathode band at pad 1. Footprints without a
  spec get a box from their body size, or a 1 mm grey box over the courtyard. Bottom-side parts are mirrored under
  the board.
- **Camera**: `view top|bottom` (the bottom view turns the board over, mirrored like the realistic bottom view),
  `azimuth` (degrees clockwise from the front edge; 45 = front-left, the default), `elevation` (5–90°, default
  35.26° = isometric; 90 looks straight down, with the board turned by the azimuth). `size` (longer side, default
  1600 px) or `px_per_mm`.
- **Hidden surfaces**: a z-buffer with 3×3 supersampling (anti-aliasing by box filter), back faces culled. The image
  is rendered in 32-row bands in parallel; triangles are drawn in a fixed order and a sample is replaced only by a
  strictly closer one, so the bytes do not depend on thread count.
- **Shading**: flat, one directional light fixed relative to the camera (upper left), 42 % ambient; background is a
  dark vertical gradient. `highlight U1,R3` paints those parts orange.
- **Speed**: the STM32 test board (60 × 45 mm, 40 parts) renders at 1600 px in well under 0.1 s (release).

Later: STEP/VRML models for accurate bodies (STEP needs a B-rep tessellator; evaluate crates then).
