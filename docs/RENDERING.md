# Rendering

cadlab has no UI. Rendering lets humans review a design, and lets multimodal agents check their own work.

## Pipeline

```
model ──► Scene (2D primitives, layers, styles) ──► SVG (vector, exact) ──► PNG (resvg / tiny-skia)
```

- One scene builder per view. SVG is the canonical vector output and PNG is a rasterization of it, so both stay
  pixel-consistent.
- Fonts are embedded in the binary (a permissively licensed stroke font, e.g. Hershey-derived, for silkscreen, plus one sans-serif for
  labels), so renders are identical on every machine.
- Output size by pixel dimensions or DPI. Region crop by bounding box or object (`--around U3 --margin 5mm`).

## Views

| View | Contents |
|---|---|
| `schematic` | auto-laid-out symbols, wires, labels, refdes/values; one image per sheet/block or combined |
| `board` | selected layers with standard colors, outline, drill holes |
| `board --realistic` | soldermask color, silkscreen, exposed copper finish, as the fab would produce |
| `ratsnest` overlay | unrouted connections as straight lines |
| `drc` overlay | markers + labels for violations |
| `highlight` | emphasize nets/components, dim everything else |
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
