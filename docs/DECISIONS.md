# Decisions

Short records of choices made, and open questions to settle. Add new entries at the end. Don't rewrite history;
supersede an entry with a new one.

## Decided

### D1. API first, generated frontends
The library's command registry is the only place operations are defined. CLI and MCP are derived from it.
*Why:* three hand-maintained interfaces drift apart; agents need the same behavior from all of them.

### D2. Netlist is the source of truth; schematic is a view
*Why:* connectivity as data is natural for programs and agents; drawing coordinates are not. See DATA_MODEL.md.

### D3. Integer nanometer coordinates (`i64`)
*Why:* exact geometry, no float drift in stored data, fine resolution with headroom.

### D4. Project = directory of deterministic, diff-friendly text files
*Why:* git-friendly, reviewable, mergeable; agents can read files directly if needed.

### D5. Cargo workspace with layered crates (superseded by D16)
*Why:* compile times, independent testing, library users pick only what they need.

### D6. No LLM calls inside cadlab
*Why:* cadlab is a tool for agents, not an agent. It stays deterministic, offline-capable and testable.

### D7. License: MIT; GPL tools are verification oracles only (2026-10-03)
cadlab is MIT. KiCad, freerouting and other GPL projects are **strictly off-limits as code sources**: no copying,
porting or translating their code, no bundling or converting their libraries (symbols, footprints, 3D models).
They may be used only as external **verification oracles** in tests/CI, run as separate processes (see TESTING.md).
File formats (KiCad, Specctra) are implemented from published documentation and observed files.
The router is designed from published algorithms (literature), not from freerouting's implementation.

### D8. Own save format, industry standards for exchange (2026-10-03)
Native project files use cadlab's own JSON format (D4). Every output for manufacturing or other tools uses
industry standards: Gerber X2/X3, Excellon/XNC, IPC-D-356A, IPC-2581, Specctra DSN/SES, IDF/IDX, STEP. Footprint
geometry and naming follow IPC-7351B. See MANUFACTURING.md.

### D9. Polygon library is a separate standalone crate (2026-10-03)
Built separately against the requirements in POLYGON_LIB.md (integer, exact predicates, robust, deterministic,
arc approximation with selectable side, vertex provenance tags). cadlab depends on it through a thin adapter in
`cadlab::geom`, so it can be swapped if needed.

### D10. KiCad interop is in, as an oracle first (2026-10-03)
KiCad writers come early (netlist in M2, schematic in M3, board in M4) so `kicad-cli` can cross-check ERC, DRC and
Gerbers from M4 on. KiCad import (user projects, migration) follows in M7.

### D11. Multi-fab support through data-driven fab profiles (2026-10-03)
JLCPCB and PCBWay first, then as many fabs as practical (OSH Park, Aisler, Eurocircuits, Seeed, NextPCB, ...),
plus generic IPC class 2/3 profiles. Fab specifics live in profile files, not code. See MANUFACTURING.md.

### D12. Projects are provider-agnostic; fab choice happens at export (2026-10-03)
Supersedes the "default fab profile" question. Projects store design intent (rules, board spec as preferences,
BOM as requirements + approved MPNs) and never depend on a fab or supplier. `fab check` / `fab compare` /
`export fab --fab X` evaluate and resolve against a fab at export time and write a `fab-lock.json` with the
concrete choices. Default rules for new projects: a conservative generic IPC class 2 set. Optional manifest
`targets` only add DRC checks.
*Why:* fabs change lead times, capabilities and parts stock; retargeting must be cheap.

### D13. No `.kicad_sch` parser; circuits are imported as netlists (2026-10-03)
Schematic import is not needed for our goals. For oracle tests on third-party KiCad projects, `kicad-cli` exports
the netlist and cadlab imports the netlist (and `.kicad_pcb` for boards). Users migrating a project do the same.
Our own `.kicad_sch` *export* stays (ERC oracle).
*Why:* parsing schematic geometry to recover connectivity is a lot of work for no product value in a netlist-first
tool (D2).

### D14. Own MCP implementation; no async runtime outside supplier I/O (2026-10-04)
Settles Q1. The MCP server is a small in-house layer in `src/bin/cadlab/mcp.rs`: newline-delimited JSON-RPC 2.0
over stdio, synchronous, a reader thread for cancellation and a main thread running requests in order. It
implements `initialize` (version negotiation over 2025-11-25 / 2025-06-18 / 2025-03-26 / 2024-11-05), `ping`,
`tools/list`, `tools/call`, progress and cancellation notifications. Resources, prompts, image content and the
streamable HTTP transport are added when needed (render in M3, HTTP when a client needs it).
Core stays sync; an async runtime may appear only inside the `suppliers` module, wrapped in a blocking API.
*Why:* the protocol surface we need is small (~500 lines), the tool schemas come straight from the registry, and we
keep full control over tool shape and output size. Avoids pulling an async runtime into the binary for a
sequential stdio protocol. Cost: we track MCP spec revisions ourselves; revisit if that becomes a burden.

### D15. Tool shape for MCP: one tool per command group (2026-10-04)
Each group (`project`, `history`, later `bom`, `board`, ...) is one MCP tool taking `{action, args, project?,
dry_run?}`; the description lists each action with a compact argument signature, and `describe` returns full
schemas. Plus generic `describe`, `call`, `batch`. Top-level `oneOf`/`anyOf` is avoided in tool input schemas
because some clients (e.g. the Claude API) reject it.
*Why:* keeps the tool list short as commands grow into the hundreds, while still exposing every command.

### D16. Single crate; spin out standalone crates when useful (2026-10-04)
Supersedes D5. cadlab is one crate (library + `cadlab` binary behind the default `cli` feature); subsystems are
modules. Generally useful pieces are extracted into their own repository and published as independent crates,
as with polyclip, rather than splitting cadlab into internal sub-crates.
*Why:* simpler to work on and to depend on; one version, one changelog. Reusable parts still get a clean,
public home.

### D17. Credentials live in user settings, entered through the CLI only (2026-10-04)
Supplier credentials are stored in `~/.config/cadlab/config.toml` (owner-only permissions), set with the
interactive `cadlab config digikey` (secret typed without echo, verified before saving), and overridable by
environment variables. There is deliberately no registry/MCP command to set them, and `config show` masks
secrets.
*Why:* secrets must not pass through an agent's context or land in projects, logs or the operation log.

### D18. Blocks are templates; the circuit stays one flat netlist (2026-10-04)
Hierarchy is provided by blocks: a block is captured from existing components (parts, internal nets,
ports). Instantiating copies the components with fresh designators, names internal nets
`<instance>/<net>` and connects ports to circuit nets; components remember their instance. There are no
nested sheets in the data model.
*Why:* every other feature (ERC, netlists, placement, routing, BOM) works on one flat netlist without
special cases, and agents can address any pin as `REFDES.PIN`. Schematic rendering can still group a block's
components (M3). Editing a block does not update existing instances; re-instantiating is explicit.

### D19. Shared libraries are copied in and out explicitly; block files are self-contained (2026-10-04)
Parts, footprints and blocks can live in shared libraries outside projects: the user library in
`$XDG_DATA_HOME/cadlab/library` (default `~/.local/share/cadlab/library`, `%APPDATA%\cadlab\library` on Windows),
then the directories listed as `libraries = [...]` in the user settings, searched in that order. A library has
the project library's layout (`parts/`, `footprints/`, canonical JSON) plus `blocks/` and a `library.toml` with
a schema version. Projects never resolve anything from a shared library: `lib.import` copies items into the
project and `lib.publish` copies them out; a missing part only gets an import hint. A block file embeds copies
of the parts and footprints it uses instead of referencing library items.
*Why:* a project must build identically when a library changes or is absent (projects are shared and archived
on their own). Self-contained block files cannot break when a referenced part is edited or removed in the
library, at the cost of duplicated part data. Conflicts are reported, never merged silently. Library data,
unlike credentials, is not secret, so it lives in the data directory rather than the config directory.

### D20. Mounting holes are board items that behave as pads; placement is heuristic and deterministic (2026-10-04)
Mounting holes are stored on the board (`board.holes`: name, center, drill, optional plated pad diameter and
net), not as components: they have no part, no BOM line and no schematic symbol. The shared geometry turns each
into a pad of "designator" = hole name (`placed_pads`), so DRC, zone fill, connectivity, rendering, Gerber,
drill, IPC-D-356 and KiCad export need no special case beyond small additions (drill function `MechanicalDrill`,
a one-pad `MountingHole` footprint in `.kicad_pcb`). Outline cutouts stay inner contours of `board.outline`.
Automatic placement (`place.auto`, strategy `groups`) is a constructive heuristic followed by greedy local moves,
on a 50 µm grid with fixed candidate orders, so the same project always gives the same placement; no
randomized annealing.
*Why:* a mounting hole is a mechanical board feature that agents add while designing the board, and making it a
pad reuses every consumer of pads. A deterministic heuristic is reviewable and stable across runs, which matters
more for an agent-driven flow than squeezing the last millimeters of ratsnest.

### D21. Fab profiles: sourced data, temporary rules, check before export (2026-10-04)
Fab profiles are TOML files (`fab-profiles/`, embedded; user files in `<config dir>/fab-profiles/` merge onto
them table by table). Every value is taken from the fab's own published pages, listed in `sources` with a
per-field `cite`; values that could not be confirmed are left out (not checked) or kept and listed in
`unverified`, and where a fab's pages disagree the stricter value is used and marked unverified. No per-package
CPL rotation offsets ship, because neither JLCPCB nor PCBWay publishes a table; the profile format supports them
and they are applied at export only. `fab.check` reuses the DRC (`drc::check_limits`) with a temporary rule set
built from the profile and net class values ignored, so the project's rules are never touched. The manifest
`targets` run the same check from `drc.run`, as warnings only. The fab-specific export is `fab.export` (in the
`fab` group with the other fab commands, rather than `export.fab`); it refuses when `fab.check` finds errors
unless `force`, and writes a deterministic zip (fixed timestamps) and a `fab-lock.json` with SHA-256 file hashes.
*Why:* fab data goes stale and is easy to misremember, so provenance must be visible per value; reusing the DRC
keeps one geometry engine; a deterministic bundle and lock make an order reproducible and diffable.

### D22. Router v1: exact sampled grid, PathFinder negotiation, DRC as the last word (2026-10-04)
The M5 router is a grid maze router designed from published work only (Lee/A* maze routing, PathFinder by
McMurchie and Ebeling 1995; D7). Legality is sampled on a half-pitch grid with obstacles inflated by
`√(r² + s²/4) − r`, which makes "all samples legal" imply "the segment is legal", so grid routes are DRC-valid by
construction; nets claim clearance halos per net-class profile and negotiate them. Post-processing only applies
changes that pass exact clearance checks, and the output is verified with `drc::check`: anything flagged is ripped
and reported, never returned silently. Results are deterministic for a given input and seed; a time budget that
cuts a run short trades that for a best-so-far legal result.
*Why:* correctness first (agents act on the output), with simple data structures that are easy to test; the
gridless push-and-shove router of M6 can reuse the model, index, checks and reports.

### D23. Large-board speedups never change results (2026-10-04)
Performance work on shared geometry (connectivity, ratsnest, DRC, zone fill, rendering) must produce
byte-identical outputs: indexes and shortcuts only skip work whose answer is known, and exact queries still
run on `polyclip`'s own predicates (`board::prepared` hands `polyclip` a windowed view of a large shape).
Equivalence tests keep the straightforward versions as references, and the synthetic 500-component board
(`tests/common/bigboard.rs`) is compared output for output before and after. Multi-threaded booleans come from
`polyclip`'s `rayon` feature behind cadlab's default `parallel` feature, since its results do not depend on the
thread count.
*Why:* fills, DRC reports, Gerbers and renders are compared by golden files and oracles, and an agent must get
the same answer from the same design on any machine. Approximate speedups (tiling a pour, clipping keep-aways
to a window) would move vertices by snap rounding and are not taken, so the remaining zone fill time is spent
in `polyclip`'s offsets (docs/BOARD.md, "Performance").

### D24. Rendered schematics span several sheets; the KiCad export stays one sheet (2026-10-04)
The schematic layout is built from groups (one per IC/connector with what attaches to it, leftover chains,
and one titled frame per block instance), which are packed onto sheets and never split. `render.schematic`
uses A4 or A3 and continues on more A3 sheets (`name-1.png`, `name-2.png`, ...). `schematic.export` packs the
same groups onto one sheet of the smallest paper that holds them (A4 to A0, then a custom size).
*Why:* sheets of at most A3 stay readable on screens and in images given to agents. KiCad is an oracle and an
interchange target here: a multi-sheet KiCad schematic needs a root sheet with sheet symbols, hierarchical or
global labels for every net crossing sheets, and per-sheet instance paths, all of which the ERC and netlist
oracles would then test instead of the layout. One flat sheet keeps local labels and the exported netlist
exactly cadlab's; hierarchical export can come later if humans need it.

### D25. Specctra: DSN out, SES in, written from the spec and checked against freerouting (2026-10-04)
cadlab exchanges routing with external autorouters through Specctra files, implemented from the Specctra
Design Language Reference and its session file description (`src/specctra/`, an algorithm module with its own
small S-expression reader/writer, since the KiCad writers are output-only and quote differently).
`export.dsn` writes the board (coordinates in µm with as many decimals as needed, so nothing is rounded;
`resolution` only sets the router's grid, default 0.1 µm); `route.import_ses` applies a session: nets matched by
name, integer resolution steps converted exactly to nanometers, the unlocked routing of the session's nets
replaced (locked items stay, and session wiring that only repeats them is not added again), unknown nets, layers
or via padstacks rejected with stable codes. Choices: pad rotation is baked into padstacks (quarter turns swap
the rectangle, other angles become polygons) rather than relying on pin `rotate`; round rectangles are polygons
circumscribing the corner arcs (never smaller than the pad); via padstack names carry diameter, drill and layer
span (`via_600_300`) because Specctra padstacks have no drill; back-side placement is written with cadlab's own
rotation, since Specctra mirrors the image across its Y axis and then rotates counter-clockwise exactly as
cadlab does (confirmed by the freerouting oracle, which fails with the opposite convention); zones are not
written and are refilled after import. The DSN reader parses every field the writer emits (round trip) and the
common forms of other writers, but there is no "import a DSN as a new project" command: designs come from
cadlab's own circuit and parts.
*Why:* freerouting is the reference router to benchmark against (D7) and a fallback for boards the M5 router
does not finish; file-level exchange keeps it an external process. Exact unit conversion and DRC after import
keep cadlab the judge of what is legal.

### D26. Rule presets and fab-derived rules store numbers; substitutes are per-fab lock entries (2026-10-04)
`board.rules` builds the rule set from a preset (`ipc2` = the defaults, `ipc3` = class 3 annular ring and
vias), then from a fab profile (`fab`, `margin` `tightest` = the fab's limits, `comfortable` = limits + 25 %
rounded up to 10 µm, default track/via never below the class 2 defaults), then from explicit fields; only the
resulting numbers are stored, with no reference to the preset or fab. IPC values whose standard text could not
be read are documented as unverified with their secondary sources (docs/BOARD.md). The DRC keeps reading net
class values over the rules, and now also warns about vias smaller than their class and class values below
the board minimums. Parts that a fab cannot source get substitute candidates from two sources only: drop-ins
listed by a provider's cross-reference data (any category; the only source for non-passives) and parametric
matches for passives (same package and value, tolerance at most, ratings at least). Ranking is fixed (drop-in
before parametric, then the usual stock/lifecycle/price/stock order, then MPN and provider). Applying one is an
explicit `fab.substitute`, which writes it to that fab's `fab-lock.json` (created before the first export if
needed); `fab.check`/`fab.export` for that fab apply and keep it, other fabs and the design are untouched.
*Why:* rules are engineering intent and must not change when a profile is edited (D12); a preset or profile is
a starting point. Guessing pin compatibility from MPN families or parameters would put wrong parts on boards,
so non-passives need explicit cross-reference data. A substitution made because one fab's catalog lacks a part
is a fab-specific sourcing choice, which is what the lock already records; `bom.approve` stays the way to make
it a design decision for every fab.

### D27. KiCad netlist import: match parts conservatively, create the rest visibly (2026-10-04)
`circuit.import` (next to `circuit.export`, in the `circuit` group rather than a new `import` group) reads
KiCad S-expression netlists with a small in-house S-expression reader (`cadlab::sexpr`, shared with later
KiCad imports). Components keep their designators; each is matched to a project part in a fixed order:
the symbol's library part name as a part ID (only with the same value), the `MPN` field, then value plus
footprint name. A part is used only if it has every pin the netlist connects (`import.pin_mismatch`
otherwise). Unmatched passives without an MPN become generic parts when the footprint name gives a chip size
(`R_0402_1005Metric`, `RESC1005X40N` → `R 10k 0402`). Everything else gets a part built from the netlist:
pins with names and types from `libparts`, MPN and manufacturer from fields, a footprint generated when the
footprint name is a package the generator knows (or a footprint of that name is already in the project).
Such a part is `created` when it got a footprint (concrete when the netlist had an MPN, generic otherwise,
like any cadlab part), else a `placeholder` that cannot be placed yet, with a warning per component
(`import.placeholder_part`) whose hint gives the `bom.replace` / `footprint.generate` + `part.set` fix. KiCad footprints and
symbols are never converted (D7): only their names are read, as hints. Other fields stay as component
properties; nets keep their names minus the root sheet prefix; single-pin `unconnected-(...)` nets are
skipped. Without `replace` the designators must be free (`import.refdes_taken`); with it the circuit is
replaced and board placements of components that come back are kept. `kicad-cli` leaves power symbols and
`PWR_FLAG`s out of netlists, so `driven` marks cannot be recovered.
*Why:* a wrong silent match is worse than a visible placeholder: agents act on the diagnostics, and a
placeholder is one `bom.replace` away from a real part. Matching only on exact IDs, MPNs and value plus
footprint keeps the result deterministic and explainable.

### D28. Router M6 phase 1: grid router extended, not replaced; fanout as copper; data-defined batches (2026-10-04)
M6 starts by measuring and extending the M5 grid router rather than writing the gridless one first. A benchmark
suite of generated boards (`tests/common/routebench.rs`: 2-layer hobby boards, 4-layer QFP/QFN boards, fine-pitch
parts with 0.1 mm rules, a BGA-144, the 500-part synthetic board) reports completion, vias, length, segments,
sharp corners, runtime and DRC errors, optionally next to freerouting run as an external process on the exported
DSN (D7, D25). Choices: BGA dog bones and fine-pitch escapes are planned before routing and enter the run as
existing copper of their nets (static obstacles for everyone, many access cells for their net), not as optional
access paths: as access-only paths they lost completion on every fine-pitch board, because negotiation then
fought over the single cell each escape offered. Escapes are absorbed into the path that uses them for
post-processing, and unused fanouts are dropped, so the output has no dead stubs. Escapes are only made for
components that need them (some pad too far off the grid for a straight exit); escaping well-aligned parts made
routes longer and more segmented for nothing. Parallel routing uses batches of nets with disjoint regions found
by level scheduling over the routing order, routed against a shared occupancy snapshot and committed in order;
the batches depend only on the data, so the output is identical for any thread count and without the `parallel`
feature (rayon). Cancellation is polled between batches, the time budget inside searches. Any-angle segments are
optional (`any_angle`), since fabs and humans expect 45° routing. "Shove" for `route.connection` is rip-up and
reroute of the whole nets reported in the way, kept only if they lose nothing; geometric push-and-shove of
segments is left to the gridless router.
*Why:* the benchmark showed where the grid router lost (off-grid fine-pitch pads, unresolvable negotiation
leftovers, same-net via spacing) and those fixes were cheaper and safer than a new router. Determinism and DRC as
the last word (D22, D23) stay non-negotiable, which rules out thread-timing-dependent merge orders.

### D29. Electrical calculations: published closed forms, exact stored values, heuristics as warnings (2026-10-04)
Impedance comes from published closed-form models, not a field solver: Hammerstad–Jensen for surface
microstrip, Wheeler (as given by Wadell) for stripline with a parallel-combination approximation for
asymmetric striplines, IPC-2141A for embedded microstrip and the IPC-2141 coupling formulas for edge-coupled
differential pairs; each formula, its source and its accuracy range are listed in docs/ELECTRICAL.md, and
inputs outside a range give a warning. Outer layers are microstrips over the adjacent layer, inner layers
striplines between their neighbours, all neighbours taken as planes. The stackup gains optional
`dielectrics` (thickness, εr as an exact decimal, material); when absent, an equal split at εr 4.5 is assumed
and every result based on it says so. Nets gain optional `voltage`, `current` and `temp_rise`, net classes
`impedance` and `diff_impedance` targets, all exact `Quantity` values; these additive optional fields need no
schema migration. Track width for a current uses a published curve fit of the IPC-2152 chart (marked
approximate, no modifiers), with IPC-2221 as an option. The SPICE export writes the ngspice dialect from part
data and never invents models for active parts: a part without `spice_model` is written commented out with a
warning, and only diodes get SPICE's default model (with a warning). The design lint is a separate command
(and an opt-in `circuit.erc --lint`) whose findings are heuristics on categories and names, so they are
warnings or notes, never errors.
*Why:* closed forms are fast, deterministic, license-clean and accurate to a few percent, which is what a
design-time width needs; fabs re-tune impedance-controlled widths with their own solvers anyway. Storing exact
inputs and targets (not computed floats) keeps files deterministic and diffable. A simulation that silently
used made-up models for ICs would mislead; a visible placeholder is one `part.set` away from a real model.
Keeping lint out of the default ERC keeps ERC results (and the M2 exit test) stable while making the lint
available to agents in one call.

### D30. 3D view: own z-buffer rasterizer, bodies from package specs, realistic render as face texture (2026-10-04)
`render.board3d` is a separate command (not a `render.board` option: none of its layer, ratsnest, marker or crop
options apply). It renders an orthographic view (isometric by default, any azimuth/elevation, top or turned-over
bottom) with a small software z-buffer rasterizer, 3×3 supersampled, flat-shaded with one camera-relative
directional light. The board faces reuse the realistic 2D render as a texture, with coverage from the outline and
drill holes so holes are see-through; walls are extruded from the outline, cutouts and holes. Component bodies are
convex solids generated from the footprint's stored `PackageSpec` (leads placed at the pads), a body box, or a grey
courtyard box when there is neither. Output is deterministic (fixed draw order, strict depth test, independent
bands); tests check identical bytes for identical input and probe pixels at projected component positions instead of
storing golden images, whose floating-point rounding could differ between platforms.
*Why:* no GPU or 3D engine dependency, MIT-clean, fast (tens of ms for the STM32 test board), and agents get a
recognizable picture of the assembly from data cadlab already has. A z-buffer is simpler and more robust than a
painter's algorithm with polygon splitting; supersampling gives anti-aliasing for free. Accurate bodies from STEP/VRML
remain a separate roadmap item.

### D31. Exchange outputs: IPC-2581C, STEP AP214 and IDF 3.0 written by hand, boxes for bodies (2026-10-04)
`export.ipc2581`, `export.step` and `export.idf` write their formats directly from the published
specifications, without XML or CAD-kernel dependencies: the files are text, the subset needed is small, and a
kernel (OpenCASCADE) would bring a large C++ LGPL dependency. IPC-2581 lives with the other fab outputs
(`src/fabout/ipc2581.rs`, included in `export.all`); STEP and IDF in a new algorithm module `src/mcad/` that
shares one description of the board: outline loops of lines and exact arcs (arc centers recovered from the
stored mid points, snapped to the roundest whole-nanometer center that fits), the drilled holes, and component
bodies. Choices: bodies are boxes from the footprint's package dimensions (`body`), centered on the footprint
origin, the only 3D data cadlab has until model import; components without one are reported, never guessed.
The STEP board is an exact B-rep (planes and cylinders), not a faceted mesh, so MCAD tools measure holes and
arcs exactly; holes that would touch the edge, a cutout or another hole are skipped with a warning rather than
producing an invalid solid, and vias are opt-in. The STEP file is an assembly (one product per footprint body,
instances named by designator) so MCAD trees show designators. IPC-2581 writes pads per layer with dictionary
primitives (no padstack definitions), uses placeholder logistic data (projects store no people), and leaves out
`HistoryRecord`/`Avl`, which need dates; STEP and IDF headers carry fixed dates. The IPC-2581 schema is not
vendored (IPC's terms; not freely downloadable at the time of writing): structure is checked by tests that parse
the output, and by `xmllint` against a user-supplied XSD when available. FreeCAD (`freecadcmd`) is an optional
oracle that must read the STEP file as valid closed solids with the expected volumes.
*Why:* MCAD and fab exchange are needed to finish a product, and these three formats cover what fabs and
mechanical engineers ask for; hand-written text keeps the crate small and the output byte-for-byte deterministic,
and exact geometry with explicit skips keeps the files trustworthy.

### D32. KiCad board import: exact geometry, the circuit stays the source of truth (2026-10-04)
`board.import_kicad` (next to `board.export_kicad`) reads `.kicad_pcb` files of KiCad 6 to 10 with the shared
S-expression reader, plus the `.kicad_pro` / `.kicad_dru` next to them (`board.import_kicad_rules` alone).
Coordinates are parsed from decimal millimeters to `Nm` exactly and converted with the exporter's inverse frame
(origin: the auxiliary axis when set, as cadlab's export writes it, else the outline's lower-left corner), so a
cadlab board comes back equal. Footprints embedded in the board are the user's design data and become project
footprints; KiCad's libraries are still never read or converted (D7). Identical instances share one footprint,
and a footprint equal (within 3 nm, any item order) to a project footprint reuses it. Shapes cadlab cannot
represent are approximated conservatively (trapezoid pads as bounding rectangles; custom pads were bounding rectangles until the
corpus, D35, made them polygon pads, and slots round holes until D40 modeled them) with a warning; items without counterpart are reported, never dropped silently. With a circuit in the
project it wins: footprints match by designator, pads to pins through the part's pin map, board nets take the
circuit's names, and disagreements are reported for the user to fix; footprints the circuit lacks are not
placed. Without one, the circuit is built from the board through the netlist importer (D27), so parts are
matched or created the same way as from a netlist. Zone fills are recomputed; zone settings and net class
values equal to cadlab's defaults are left unset so they keep following the rules. KiCad's rule areas map to
keep-outs, its Default class and board minimums to `board.rules`, other classes to net classes.
*Why:* migration is only useful if nothing moves: exact parsing and the inverse of the tested exporter make the
round trip lossless, and the KiCad DRC cross-check on imported boards proves it. Keeping the circuit
authoritative (D2) means an import never silently rewires a design; building it from the board covers users who
have only a board.

### D33. Mouser and Nexar providers; LCSC/JLCPCB through imported parts lists (2026-10-04)
Mouser (Search API key) and Nexar/Octopart (GraphQL, client credentials) are network providers like DigiKey:
behind `net`, cached, credentials entered only through `cadlab config mouser|nexar` (D17), implemented from
Mouser's published OpenAPI description and Nexar's documentation and published schema. Network providers send
requests through a `supplier::http::Transport`, so tests use a mock transport and fixtures. Nexar returns one
candidate per seller offer with a `<seller>:<sku>` SKU, brokers excluded and unauthorized sellers opt-in;
neither Mouser's suggested replacement nor Octopart's similar parts count as drop-ins (D26). LCSC and JLCPCB
have APIs, but only for approved partners, with non-public documentation and (LCSC) terms forbidding sharing
technical aspects with third parties: cadlab ships no client and never scrapes. `catalog.import` instead turns a
CSV parts list the user downloads into an offline `lcsc` catalog, matched by header names (neither publishes a
stable export format), with explicit mapping for anything else and parameters of passives read from the
description; it writes to the user catalog directory, outside projects. PCBWay's partner API has no parts
search; PCBWay sources by MPN.
*Why:* the fab profile already orders JLCPCB parts by LCSC SKU, and a user-provided list is the only source
that is both permitted and reproducible. Header recognition plus explicit mapping survives export format
changes without guessing, and reading values only for passives keeps parametric matching (D26) honest.

### D34. Router M6 phase 2: shove geometry, keep the grid search, refine off the grid (2026-10-04)
The grid router of D22/D28 stays the search engine; phase 2 adds geometric stages around it, each checked
exactly and by the DRC, so none can make a result illegal. *Push-and-shove* (`router::shove`) works on board
geometry, not on the grid: unlocked straight tracks of other nets become lines between anchors, obstacles are grown
into convex hulls widened by an octagon whose inradius is the required distance (+0.5 µm), lines are walked around
hulls (hull boundary between first entry and last exit) and vias pushed out of them, moved items push in turn
(FIFO, bounded), then spring back (original geometry if legal, else 45° pull-tight). Locked items, pads, arcs,
mixed-width lines and the pushing net's own copper never move; a fixed end or a crossing in the way is a failure
with a code, the item and a hint, never a silent partial result. It serves `route.track` (waypoints; modes shove,
walkaround, strict), `route.connection` (routed again against *softened* movable copper, first touching allowed,
then centerline only, then shoved; the D28 rip-and-reroute stays as fallback) and the leftovers of `route.all` /
`route.nets` (one attempt each within the budget). *Gridless refinement* is a post-pass, not a new search: a
visibility graph over the clearance hulls near each stretch of a path, A* with lazily checked octilinear (or
straight, with `any_angle`) edges, replacing the stretch only when shorter; it is the default because the
benchmark showed shorter tracks and fewer segments on every board with no completion or DRC change. *Via
minimization* reroutes nets with vias after legalization with vias four times as dear and keeps the result only
if it has fewer vias, no new failures and bounded extra length; raising the via cost during negotiation instead
lost completion on the STM32, QFP and BGA boards. *Arcs* are optional corner rounding (`arcs`, `arc_radius`), checked as chords
within 0.1 µm with that much extra clearance and stored as the track's midpoint, which every writer already
exports. Escape planning retries a component with the pads that found no exit first. Everything is sequential
or batch-deterministic: results do not depend on the thread count.
*Why:* the benchmark gaps were local (one blocked escape, one congestion leftover, cheap vias), and the
interactive use case (agents adding one track without destroying the rest) needs geometric shove, which does not
fit a grid occupancy model. Refining off the grid gives most of a gridless router's quality (hugging minimum
clearance, true shortest corridors) without giving up the grid search's robustness and exact sampling (D22);
a gridless search remains on the roadmap.

### D35. Open-source corpus: pinned, fetched, never vendored; differences explained or counted (2026-10-04)
Third-party KiCad projects test the importers and the oracle cross-checks on real boards. Selection: KiCad 6+
board files (with the root schematic when there is one), a license that allows redistribution (permissive or
open hardware: MIT, 0BSD, Apache-2.0, Unlicense, CERN-OHL-P/-W, SHL; recorded per project; no GPL code
projects), varied on purpose (2 to 6 layers, KiCad 6 to 10 formats, SMD and THT, BGAs, pours, keep-outs,
custom pads, net ties, custom rules, board-only and hierarchical designs, one board without a schematic).
Each is pinned to a full commit SHA in `tests/corpus/projects.toml` with the sparse paths to check out;
`scripts/fetch-corpus.sh` does a shallow, blobless, sparse fetch of exactly that commit, verifies `HEAD`, and
puts it outside the tracked files (`target/corpus`, or CI's cache keyed by the manifest's hash). Tests never
download and skip without `CADLAB_CORPUS_DIR`; they work on a copy, so a checkout is never modified. Updating a
pin is a reviewed manifest change. Every difference between cadlab and KiCad must be matched, covered by a
global allowance with a reason (rules only one tool has, report granularity, report truncation, rules the
project sets to ignore), or listed for that board with its reason and an exact maximum count; stale entries
fail, so the manifest follows fixes. Gerber copper is gated without pours (each tool fills differently);
silkscreen and routing completion are reported only. The corpus job is informative (`continue-on-error`) like
the oracle job.
*Why:* real boards exercise what generated ones do not (custom pads, holes in keep-outs, net ties, stitching
vias, mask margins), and they found eleven genuine bugs in the first pass. Pinning makes results
reproducible per KiCad minor version; not vendoring keeps third-party designs and their licenses out of the
repository (D7); exact counts turn known differences into regression tests instead of a blanket allowlist.

### D36. 3D models come from oxideav-mesh3d (2026-10-04)
cadlab reads 3D model files only through the **oxideav-mesh3d** crate family (MIT): the typed `Scene3D` model with
its `Mesh3DDecoder` trait and `Mesh3DRegistry`, and the format crates `oxideav-stl`, `oxideav-obj`,
`oxideav-gltf` and `oxideav-usdz`, behind the default cargo feature `models3d`. cadlab writes no mesh types or
3D parsers of its own: `cadlab::models3d` builds the registry (`build_registry`, the single place a format crate
is registered), decodes by file extension, and walks the decoded scene (`world_node_transforms` /
`world_mesh_with`, `triangle_indices`, material base colors) into placed triangles for the renderer and the
exporters. STEP and VRML decoders are being added to the family; they plug in with one dependency line and one
`register` call, nothing else changes. Until then `.step`/`.wrl` give `model.unsupported_format` with the
formats available now.
Models attach to footprints, or to a part's footprint reference (an override for that part). The stored
reference is exact (offset in `Nm`, rotations in millidegrees about X, Y, Z, per-axis scale in ppm, optional
unit and up-axis overrides); the model file is project data, copied as-is into `library/models/` (D19: projects
never resolve files outside themselves), carried base64 in packed projects, undo snapshots and block files, and
copied by `lib.publish` / `lib.import` as library items of kind `model`. Unused files go away with their last
reference. Renders draw model triangles double-sided; STEP export writes them as faceted B-rep (closed,
outward-oriented `FACETED_BREP` pieces, or a surface model when a mesh is open); IDF takes the model's bounding
box. A model that cannot be read never fails a render or an export: the generated body is used, with a warning.
*Why:* the oxideav crates are the user's own MIT 3D layer with a typed scene model and pluggable decoders, so
cadlab gets STL/OBJ/glTF/USDZ now and STEP/VRML later without carrying parsers (or KiCad's GPL-adjacent tooling)
itself. Storing the file in the project keeps projects self-contained and undo/dry-run exact; exact placement
values keep files deterministic and diffable. Meshes are what every format decodes to, so one path serves all
of them; faceted B-rep is the standard AP214 way to carry a mesh as a solid.

### D37. IDX baseline export; no ODB++ (2026-10-04)
`export.idx` writes an IDX (ProSTEP iViP PSI 5, EDMD schema V4.5, namespaces `.../edmd/4.0/...`) baseline
(`SendInformation`) from the free recommendation, implementation guidelines and schema, which prostep ivip
publishes "for anyone to use", duplicable "for use in the context of creating software". It lives in
`src/mcad/idx.rs` on the shared outline loops, hole list and package bodies (D31). Choices: every feature is an
assembly item with the IDX 4.0 `GeometryType` *and* a single item whose shape is the classic classification
object (`Stratum`, `InterStratumFeature`, `KeepOut`, `AssemblyComponent`), so readers of either method work;
components use absolute 3D transforms (top face, or turned over on the bottom face) rather than the layer-relative
"passive" model, since cadlab sends no layer stack-up; a cadlab keep-out becomes one item per forbidden kind
(routing, via, plane, and component placement per side, unbounded away from the board); holes share padstack items
per kind and diameter; identifiers are derived from names (designators, hole and keep-out names) so successive
baselines agree; time stamps and creator fields are fixed or empty. Components without package body data are
reported and left out, as in STEP and IDF. The schema is not vendored (redistribution only unchanged, with its
notice); an optional test validates with `xmllint` against a user-supplied copy (`CADLAB_IDX_XSD`). Incremental
`SendChanges` messages wait until cadlab can track and accept MCAD-side changes.
**ODB++ is not implemented**: the Siemens specification (8.1 update 4, 2024) is confidential documentation that
"may not be used in any way not expressly authorized by Siemens", downloading it "does not grant a license to
develop software interfaces" (v7 notice), and the only license offered (ODB++ Solutions Development Partnership)
is nontransferable, non-sublicensable and tied to the partner's products. Quotes and URLs in MANUFACTURING.md.
*Why:* IDX is the open, incremental ECAD-MCAD exchange MCAD tools (Creo, NX, SolidWorks PCB and others) import, and
a validated baseline is the part cadlab can produce faithfully today. ODB++ would bind cadlab, and everyone who
reuses its MIT code, to a proprietary license it cannot pass on; IPC-2581 (D31) carries the same fab data openly.
KiCad's ODB++ export is not an oracle to build against for the same reason (D7).

### D38. Differential pairs routed as one centerline; length tuning by closed-form meanders (2026-10-04)
Differential pairs and length groups are circuit data (`circuit.json` `diffpairs`, `length_groups`; optional
fields, no migration), provider-agnostic, with lengths and limits in `Nm`. A pair's geometry comes from a net
class (`diff_pair_width`/`diff_pair_gap`, written by `impedance.solve`), so the impedance solved for a layer is
what gets routed; pair nets use the pair width everywhere and the DRC accepts the pair gap between the two
nets. Pairs are routed by a dedicated A* on the pair's centerline (fat capsule including the 45° miter reach,
0°/45° moves, polarity in the state) and split by offsetting, rather than by routing two nets and pulling them
together: coupling is then exact by construction (gap within nanometers) and skew only comes from bends and
breakouts. A pair stays on one layer with no vias in v1, since the impedance and the via transition (two vias,
return path) need more than the grid router models; breakouts are straight stubs priced at twice their length.
The pair's topology is planned once for both nets (pads matched into ends, spanning tree over ends,
flow-through links on one component first) so both nets follow the same path. `route.all` routes pairs
first, and push-and-shove never moves pair copper. Length tuning inserts trombone, accordion or sawtooth bumps
on straight tracks, exact-checked and halved until they fit, with the last bump's height solved from the closed
form of its added length, so targets are met to vertex rounding; pairs are meandered on their centerline
(both nets gain the same length), skew is compensated by 45° bumps on the shorter net near where it lost
length. Lengths are tracks along arcs plus via spans between the outermost layers used, through the stackup
(the effective dielectrics, assumed when unspecified like the impedance calculator), so they are
deterministic and need no extra user input; a net's length counts all its copper. Pair and length checks are
DRC warnings with stable codes (`drc.diffpair_gap`, `_width`, `_uncoupled`, `_skew`, `drc.length_mismatch`):
they are design intent, not manufacturability. Everything is sequential and deterministic, and the DRC still
has the last word (flagged pair connections and meanders are removed and reported).
*Why:* agents need pairs that come out right the first time at the solved geometry and a single command that
closes length budgets with a report of what remains; a centerline router reuses the exact checks and index of
D22/D34 with little new machinery, and closed-form meanders make the residual error negligible instead of
iterating on measured lengths.

### D40. Local pad settings, net ties, copper drawings, slots and scoped rules are design data (2026-10-04)
The import gaps the open-source corpus measured (D35) are modeled in cadlab rather than approximated at
import. Footprints and their pads carry optional local settings (`Overrides`: mask margin, paste margin
and ratio, clearance, zone connection; a pad's value wins over its footprint's, which wins over the
board's), net-tie pad groups, SMD pads on the back of their footprint, a per-side mask opening choice
(`mask`: pad, front, back, none), slotted holes (`slot`: the hole's size along the pad axes, the drill
staying its width so code that knows only round holes stays conservative), paste-in-hole (`paste:
pad`), and copper, mask and paste drawings (on either side). The board gets `min_clearance`,
`mask_expansion`, `mask_min_web`, `paste_margin` and `paste_ratio` in its rules, filled polygon
graphics, copper graphics (netless copper), rule areas (keep-outs forbidding nothing) and custom rules
scoped by item kind, copper layer, footprint pattern, courtyard and area. All are optional with serde
defaults (no migration), stored in `Nm` or exact decimals (`Scale`), and none names a fab: they are the
designer's choices, applied by every consumer through the shared geometry (`board::placed_pads`,
`copper_items`, `board::clearance`), so DRC, zone fill, Gerber, drill, IPC-2581, IPC-D-356, Specctra,
rendering and the KiCad export agree. Semantics were taken from KiCad's documented behavior and checked
by observing `kicad-cli` on small test boards (no code read): a local clearance replaces the net class
clearance of both items (the larger local value wins, the board minimum still applies); custom rules
outrank local values and the last matching rule wins; paste is resized per axis by margin + ratio × side
with rounded rectangles keeping their corner ratio, while mask openings are exact offsets; the minimum
web is the morphological closing of the openings (round joins); a net tie lets copper of one group of
one footprint touch (its drawings bridge the group's nets) but keeps the nets apart for connectivity,
and any other copper touching a tied pad is still a short. Slots are routed in the XNC drill files
(`G00`/`M15`/`G01`/`M16`/`G05`, the documented XNC route mode) and drawn in X2 drill files. KiCad's
custom rules import when their conditions are conjunctions of the supported terms; everything else is
reported, never dropped silently. Left for later: per-layer pad stacks (front shape kept), footprint
texts, zones inside footprints, rule severities and exclusions, inner-layer footprint copper.
*Why:* the corpus showed these settings change real outputs (mask openings, stencils, neck-down
clearances, Kelvin resistors, card-edge pads) by more than the comparison tolerances, and an import that
drops them changes the design; modeling them once in the shared geometry fixes every output at the same
time and makes them editable (`footprint.set`, `board.rules`, `board.custom_rule`), while the KiCad
export writes them back so the KiCad DRC oracle checks them.

### D41. User KiCad libraries: footprints converted like board footprints, symbols as pins plus generated drawings (2026-10-04)
`footprint.import_kicad` (`.kicad_mod`, `.pretty`) and `part.import_kicad_sym` (`.kicad_sym`) bring the user's
own KiCad 6+ libraries into the project; `lib.import_kicad` puts them into a shared library (D19), writing
outside the project like `lib.publish`. They are commands of the groups whose items they create, next to
`footprint.generate` and `part.create`. Footprints go through the board importer's footprint conversion
(D32), refactored into one function used by both, so a footprint imported from a library and the same
footprint read from a board are identical; a library file's `at` is ignored, as KiCad does. A footprint is
named after its file (what a symbol's `Lib:Name` Footprint field refers to). Symbols keep their pin data
(number, name, electrical type, side, unit, alternate functions, the last two new optional `Pin` fields) and
fields; their drawing is not converted: cadlab symbols are generated from pins (D2: the schematic is
presentation), and KiCad graphics (arbitrary polylines, arcs, De Morgan styles, pin lengths off cadlab's grid)
have no counterpart in the symbol model. Each pin keeps the side it had in KiCad, so the generated box keeps
the author's arrangement; multi-unit parts are one body for now, with each unit's pins together. Derived
symbols take the root's pins and their own fields, inheriting only the standard ones, which matches what KiCad
writes (`sym upgrade`). Power symbols are skipped: in cadlab they are nets. Categories come from the reference
prefix refined by keywords, values become typed parameters when they read as one, MPN and manufacturer
fields are recognized under their usual names, distributor fields are dropped (D12), other fields become
parameters. Footprints record a provenance (new optional field) like parts, with the license the user gives.
Re-importing is idempotent (`unchanged`); a different item of the same name is a conflict listed in full
unless `replace`, and a 3D model attached in cadlab survives a replace. KiCad's shipped libraries are never
read or converted (D7); tests use hand-written fixtures and cadlab's own exports, with `kicad-cli fp/sym
upgrade` as the oracle that KiCad reads the same thing.
*Why:* migrating users bring their own vetted footprints and pinouts; footprints matter at the copper level
and are converted exactly, while symbol graphics are presentation that cadlab regenerates anyway. One
conversion path for board and library footprints keeps the corpus-tested fixes (custom pads, paste windows,
courtyards) in both and avoids two behaviors for the same KiCad item.

### D42. Spatial index and second performance pass, still byte-identical (2026-10-04)
cadlab has its own small static R-tree (`geom::RTree`: sort-tile-recursive packing over integer boxes,
fan-out 16, queries return indices sorted) rather than `rstar`: it is ~250 lines, needs no dependency, works
on `polyclip::Rect`, and answers exactly what a linear scan would, so no result can depend on the tree's
shape. It is used where it measured faster: zone fill gives each zone only the items, NPTH holes, keep-outs and
parts of earlier fills that can reach its outline (far polygons dropped and far holes filled before growing
an earlier fill; far same-net gaps and thermal pads skipped; pour rings meeting no thermal window left out of
the spoke clips), and the DRC indexes the outline's segments so that only items near the outline ask
`polyclip::contains`/`distance_less_than` on the whole contour (an item no segment comes near is located by
the exact even-odd parity of one point). Where it did not help it is not used: the DRC pair grid lists
candidate pairs faster than an R-tree self-join; the router's bucket grid changes while routing; placement
spends its time in ratsnest costs, not validity checks (that MST now runs in one pass per step, with net costs
reused across swap candidates); `islands` already sweeps sorted boxes. The fill cache keeps its inputs and
compares them (the board by pointer first) instead of serializing them. As in D23, every output (fills, DRC,
Gerbers, drill, IPC files, renders, KiCad and Specctra exports) of the synthetic board, two variants and 12
corpus boards is compared byte for byte before and after (`--example bigboard dump`), and the replaced
algorithms stay in tests. `polyclip` moved to 0.0.4 (identical outputs).
*Why:* the shapes left out are farther from a zone (or the board edge) than any distance that can change the
result, so culling them skips work whose answer is known, which D23 allows; clipping shapes to windows would
add edges and move vertices by snap rounding, which it does not. The remaining zone fill time is inside
`polyclip`'s offsets and booleans; what it would need is measured in docs/POLYGON_LIB.md §8 instead of being
worked around in cadlab.


## Open questions

None currently.
