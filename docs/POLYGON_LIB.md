# Polygon library requirements

cadlab needs a 2D polygon geometry crate. It is built as a **separate, standalone crate** (not cadlab-specific,
publishable on its own) under MIT (or MIT OR Apache-2.0). This document is its specification: what cadlab needs,
why, and how it will be verified.

Placeholder name in this doc: `polyclip`.

## 1. Who uses it in cadlab

| Consumer | Operations |
|---|---|
| **Zone fill** (copper pours) | zone outline − ⋃(obstacles inflated by clearance); thermal relief spokes; min-width enforcement (deflate→inflate); island detection and removal; fracture for Gerber output |
| **DRC** | min distance between copper shapes, overlap tests, courtyard intersections, copper-to-edge distance, containment in board outline |
| **Silkscreen** | clip open polylines and text strokes against soldermask openings (open path − polygon) |
| **Footprints/pads** | build pad shapes (rounded/chamfered rects, ovals, trapezoids, custom), paste reduction (negative offset), mask expansion (positive offset) |
| **Board outline** | assemble edges into closed polygons, validate (closed, simple, holes inside outer) |
| **Router v2** | union of inflated obstacles per layer, free-space decomposition (trapezoids/convex pieces) |
| **Rendering / 3D** | triangulation of polygons with holes; outline extrusion |
| **Gerber / IPC-2581 output** | hole-free regions (fracture), arc reconstruction for compact output |

Typical scale: a dense 4-layer board has 10⁴–10⁵ shapes per layer. A zone fill subtracts a few thousand inflated
obstacles from one outline, and is re-run often (after every routing change), so batch performance matters.

## 2. Number model

- **Coordinates:** integer, `i64`. cadlab's unit is the nanometer.
- **Supported range:** at least |x|, |y| ≤ 2⁴⁰ (≈ 1.1 km in nm, far beyond any panel). The crate documents its
  exact bound and returns an error, never a wrong answer, for inputs outside it.
- **Exact predicates:** orientation, intersection, point-in-polygon and distance comparisons are computed exactly
  (e.g. `i128` arithmetic). Floats may be used for speed only with an exact fallback, and must never change a
  topological decision.
- **Output vertices** are integers. New vertices (intersections, offsets) are rounded with a documented rule. The
  output must stay **valid after rounding**: no self-intersections, no edge crossings, correct nesting (snap
  rounding or equivalent). Rounding displacement ≤ 1 unit, unless documented otherwise for an operation.
- Generic over `i32`/`i64` would be nice but is not required.

## 3. Data types

```rust
Point { x: i64, y: i64 }
Path       = Vec<Point>              // open polyline
Ring       = Vec<Point>              // closed, implicit last→first edge
Polygon    { outer: Ring, holes: Vec<Ring> }
PolygonSet = Vec<Polygon>            // disjoint polygons ("multipolygon")
PolyTree                             // full nesting: outer → holes → islands inside holes → ...
```

Plus an input-side **shape** type with native arcs and circles:

```rust
enum Segment { Line(Point), Arc { mid: Point, end: Point } }  // or center/angle form
Shape { contour: Vec<Segment>, holes: Vec<Vec<Segment>> }
Circle { center, radius }
```

### Canonical form (output)

All outputs are normalized so results are deterministic and comparable:

- Outer rings counter-clockwise, holes clockwise (Y-up convention), documented.
- No duplicate consecutive vertices, no zero-length edges. Collinear vertex removal on by default, can be disabled.
- Each ring starts at its lexicographically smallest vertex (min x, then min y).
- Polygons in a set are sorted by a defined key (e.g. bounding box min, then area).
- Bit-identical output across platforms, runs and thread counts for identical input.

## 4. Required operations

### 4.1 Arc/circle approximation

- Convert `Shape`/`Circle` to rings with a maximum chord error (sagitta) tolerance.
- **Approximation side is selectable:** `Outside` (polygon contains the true arc), `Inside` (contained by it), or
  `Nearest`. This matters for safety: obstacles are approximated outward and fill areas inward, so approximation can
  never create a clearance violation.
- Vertex count derived from radius and tolerance, deterministic.

### 4.2 Boolean operations

- Union, intersection, difference, xor.
- Fill rules: even-odd, non-zero, positive, negative.
- Subject and clip are each a set of rings in any orientation, possibly self-intersecting or overlapping.
- **N-ary union** of many shapes in one pass (sweep-based, not pairwise folding).
- **Open path clipping:** open subject paths against closed clip polygons (inside / outside parts), for silkscreen.
- Output as `PolygonSet` or `PolyTree` on request.

### 4.3 Offsetting

- Polygon offset (positive = grow, negative = shrink), with holes handled correctly.
- Join types: round (with arc tolerance and side choice as in 4.1), miter (with limit), bevel/square.
- **Open path offset** (tracks → stadium shapes): end caps round, square, butt.
- Large negative offsets that make shapes vanish or split must produce correct (possibly empty or multiple)
  results.
- "Opening" helper: deflate by d then inflate by d (min-width enforcement for zones).

### 4.4 Vertex provenance (Z-tags)

Each input vertex/edge can carry a user tag (`u32`/`u64`). Output edges keep the tag of the input edge they came
from, and new intersection vertices record both source tags. cadlab uses this to:

- **Reconstruct arcs** after booleans/offsets (consecutive vertices tagged with the same arc ID → emit an arc in
  Gerber/IPC-2581 instead of hundreds of segments).
- **Explain DRC/fill results** ("this edge of the fill comes from the clearance around U3 pad 7").

### 4.5 Queries

- Signed area (exact, `i128` doubled area), centroid, bounding box.
- Point location: inside / outside / on boundary.
- `intersects(a, b)` and `contains(a, b)` with fast early exit.
- **Minimum distance** between any two of: point, segment, path, ring, polygon (with holes). Exact squared distance
  plus the closest-point pair. Also a thresholded form, `distance_less_than(a, b, d) -> bool`, faster than computing
  the full distance. DRC calls this millions of times.
- Validity check with a reason: self-intersection at (x, y), wrong nesting, degenerate ring, etc.

### 4.6 Utilities

- **Fracture:** convert a polygon with holes into a single hole-free outline using zero-width cut-ins
  (Gerber regions need this). Deterministic cut placement, cuts axis-aligned where possible.
- Simplify: tolerance-based (Douglas–Peucker style) while preserving topology (no new intersections).
- Triangulation of polygons with holes (constrained, for rendering and 3D extrusion).
- Convex hull.

### 4.7 Nice to have (later, not blocking)

- Trapezoidal or convex decomposition of free space (router v2).
- Minkowski sum of two polygons (non-round clearance shapes).
- Native arc-preserving booleans (avoids approximation entirely; hard, only if it comes naturally).
- Incremental/reusable engine: add/remove clip shapes and recompute only affected regions (incremental zone refill).

## 5. Non-functional requirements

- **Never panics** on any input: degenerate rings, all points collinear, zero area, duplicate rings, coincident
  edges, touching vertices, huge vertex counts. Invalid input gives an error or a well-defined result, never UB or a
  crash. Fuzzed with `cargo-fuzz`.
- **Deterministic** as described in §3.
- **Thread-safe:** no global state, types are `Send + Sync`, engines reusable across calls to avoid reallocations.
- **No `unsafe`**, or tightly scoped and justified.
- **Dependencies:** minimal. `serde` behind a feature flag. Optional `rayon` feature for internal parallelism, which
  must not affect output.
- **Performance targets** (indicative, single thread, recent laptop; to confirm against Clipper2):
  - Union of 50 000 circles (64 vertices each, heavily overlapping): < 300 ms
  - Difference: one 100 mm × 100 mm zone − 5 000 inflated obstacles: < 50 ms
  - `distance_less_than` between two 64-vertex polygons: < 1 µs on average with bbox rejection
  - Offset of a 10 000-vertex polygon: < 10 ms
- **Docs:** every operation documents its orientation, rounding and degenerate-case behavior.

## 6. Verification

- **Property tests** (`proptest`): area(A∪B) + area(A∩B) = area(A) + area(B) (exact for non-rounded cases);
  A∩B ⊆ A; (A−B) ∪ (A∩B) = A; output validity (simple, correctly nested, canonical) for random inputs;
  offset(offset(A, +d), −d) ⊇ A (approximately, per tolerance).
- **Differential testing against Clipper2** (Boost Software License) as an **oracle only**, run in tests/benches,
  never linked into the library. Compare by area and symmetric-difference area within rounding tolerance.
- **Fuzzing** of all public operations, run continuously in CI.
- **Real-world corpus:** zone-fill inputs dumped from cadlab boards, checked into the crate's test data.
- **Benchmarks** with `criterion`, tracked across releases.

## 7. API sketch (non-binding)

```rust
let obstacles: Vec<Ring> = pads.iter()
    .map(|p| offset_shape(&p.shape, clearance, Join::Round, ArcTol::new(1_000, Side::Outside)))
    .flatten()
    .collect();

let fill: PolyTree = Boolean::new()
    .subject(&zone_outline, FillRule::NonZero)
    .clip(&obstacles, FillRule::NonZero)
    .op(Op::Difference)
    .execute_tree()?;

let fill = opening(&fill, min_width / 2, ArcTol::new(1_000, Side::Inside))?;
let gerber_regions: Vec<Ring> = fill.polygons().map(fracture).collect();

if distance_less_than(&track_a, &pad_b, rules.clearance) { /* DRC violation */ }
```
