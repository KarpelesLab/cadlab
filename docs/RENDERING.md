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
| `schematic` | auto-laid-out symbols, wires, labels, power symbols, refdes/values (`render.schematic`) |
| `symbol` | one part's symbol (`render.symbol`) |
| `footprint` | pads, paste windows, courtyard, silkscreen, fab outline (`render.footprint`) |
| `board` | selected layers (`--layers`, default all copper + silk + outline) in viewer colors on a dark background: copper semi-transparent and unioned per layer (front red, back blue, inner amber/green/purple/...), silk white (front) / yellow (back), fab grey, courtyard magenta, mask/paste openings, outline yellow, drill holes; bottom-side footprints mirrored on `B.*` layers; refdes at the footprint origin on its silk layer, sized to the courtyard (`render.board`, `src/render/board.rs`) |
| `board --around U1` | the same, cropped to a component's courtyard plus `--margin` (default 3 mm) |
| `board --realistic top\|bottom` | solder mask over laminate and copper, silkscreen, exposed pads in the finish color (ENIG gold, HASL silver, OSP copper), colors from the `board.setup` preferences; the bottom view is mirrored as seen from below |
| `ratsnest` overlay | unrouted connections as thin straight lines (on by default in `render.board`, `--ratsnest false` hides) |
| `drc` overlay | markers + labels at given points (`render.board --markers '[{"at": ["5mm", "3mm"], "label": "clearance"}]'`) |
| `highlight` | emphasize nets/components (`render.board --highlight GND,U1`), dim everything else |
| `placement` | courtyards, refdes, orientation markers only: fast layout review |

Annotations (dimensions, grid, scale bar, legend) are optional so agents can read real distances from images.

## Isometric 3D (M9)

Without a full 3D engine:

- Board as extruded outline with layer thicknesses, mask and silk as textured top/bottom faces (2D render
  projected).
- Component bodies as simple solids generated from package dimensions (boxes, cylinders, pins), which are already
  known from the footprint generator.
- Isometric/orthographic projection, painter's algorithm or simple z-buffer software rasterizer, flat shading.
- Later: load STEP/VRML models for accurate bodies (STEP needs a B-rep tessellator; evaluate crates then).
