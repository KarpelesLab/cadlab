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
rounded up to 10 µm (silk limits as published since D45), default track/via never below the class 2 defaults), then from explicit fields; only the
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
Impedance comes from published closed-form models (a field solver became an option in D49): Hammerstad–Jensen for surface
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
represent are approximated conservatively (custom pads were bounding rectangles until the
corpus, D35, made them polygon pads, slots round holes until D40 modeled them, trapezoid and chamfered pads
bounding or rounded rectangles and drill offsets ignored until D44) with a warning; items without counterpart are reported, never dropped silently. With a circuit in the
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
exporters. STEP (`oxideav-step`) and VRML (`oxideav-vrml`) joined the family later and plugged in exactly that
way, one dependency line and one `register` call each (2026-10-17).
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

### D39. Router M6 phase 3: a gridless search beside the grid, `auto` by default; neck-downs and small dog-bone vias (2026-10-04)
The last M6 item is a shape-based search (`router::rooms`, `router::expansion`), added beside the grid search
(D22/D28/D34) rather than replacing it. Free space is decomposed per net, layer and search region: the
clearance regions of the obstacles (outer polygons: octagon-widened convex hulls, per triangle for non-convex
shapes) are subtracted from the board exactly on integers by `polyclip`, and the result is cut into trapezoids by
its vertical decomposition (`polyclip::trapezoids`), larger ones into a lattice of at most six grid pitches. These
convex rooms make every edge of the search (two points in one room) legal by construction, and the funnel
algorithm pulls the found channel tight without leaving it, so the search needs no sampling margin and reaches
any position and angle the rules allow (off-grid pads, rotated parts, gaps the grid cannot see). It plugs into the
existing machinery instead of having its own: wires may be grid paths or polylines; a gridless wire claims its
halo on the same occupancy maps (with the off-grid margin), samples the same PathFinder present and history costs
along its edges (lazily), and its conflicts are computed exactly against an index of every committed wire; fanout
copper, push-and-shove, pairs, tuning, post-processing and DRC verification see ordinary paths. Negotiation
searches rooms of the static obstacles (cached per net); exact searches (legalization, retries) build rooms that
also keep away from the other nets' routing in a region around the connection. `router: grid | gridless | auto`
(an option of `route.all`, `route.nets` and `route.connection`); `auto`, the default, is the grid router followed by
an exact gridless search for every connection it left unrouted, then a gridless rip-up and retry (a probe that keeps every static rule but may cross
other nets' routing names at most 8 nets to rip and reroute; kept only when fewer connections fail). The
benchmark decided it: `gridless` alone routes the generated boards less completely and much slower than the grid
(their pads are grid-aligned by construction), while `auto` never does worse than the grid and closes real-board
gaps the grid cannot; running the gridless search inside negotiation for what the grid could not reach slowed
the large boards' iterations without a completion gain and was dropped. The open-source corpus showed the other root causes, fixed in the same way as rules the
router now honors instead of failing: *neck-downs* (a net whose class width cannot leave a pad between its
neighbors, e.g. a 0.25 mm class at a 0.4 mm QFN, routes at the widest width that can, at least the board's
`min_track_width` and at most pitch minus clearance, then every segment is widened back to the class width where
it fits; what stays narrow is a `drc.track_width_class` warning) and *small dog-bone vias* (the minimum drill and
annular ring where the class via does not fit between BGA balls; `drc.via_size_class` warns). Everything stays
sequential or batch-deterministic, independent of the thread count, and the DRC has the last word.
*Why:* the grid's sampling is what made its results exact and simple (D22), and it still routes the generated
boards fastest; real boards fail where geometry is off the grid or where the rules need a narrower track or via
than the class gives. A gridless search used where the grid cannot go, sharing the negotiation, gets the
completion without giving up the grid's speed or any guarantee, and keeping the two searches on one occupancy
model, one index and one wire type is what lets them negotiate with each other.

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
reported, never dropped silently. Left for later: per-layer pad stacks (front shape kept; still open after D44), footprint
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

### D43. Benchmarks tracked in CI: exact router baseline, timings with a machine-relative rule (2026-10-14)
A blocking `bench` CI job runs the router benchmark and the large-board timings in release and compares them with
committed baselines in `tests/golden/bench/` (`route.json`, `perf.json`, rewritten by `CADLAB_BLESS=1`),
uploading JSON and Markdown reports. Router: every generated board except `big-4l` is routed with a budget it
never reaches, so its result is deterministic (identical on macOS and Linux and with any thread count), and the
check fails on any DRC error, an exhausted budget, a completion drop, more than max(2, 5 %) extra vias or 2 % extra
length; `big-4l` stays out because it is budget-bound and its result depends on the machine. Timings: one rayon
thread (`RAYON_NUM_THREADS=1`) so parallel steps do not scale with the runner's cores, best of 3, and a step fails
only when it is more than 250 ms slower than its baseline and either 3× slower than the run's median ratio to the
baseline or 8× slower outright; debug builds never fail on time. The default `cargo test` only checks the
baseline's board list and routes the smallest board exactly.
*Why:* routing quality is deterministic, so it can be held exactly where it matters (completion, DRC) with slack
only for trade-offs (vias, length) that a deliberate improvement may make. Absolute times are not comparable
between a development machine and shared CI runners, but a single step regressing stands out against the run's
own median, which cancels the machine's speed; the absolute 8× limit catches a uniform slowdown only when it is
gross, which is the price of a job that does not flap. Making the timings blocking with such a rule, rather than
purely informative, means a pathological regression (a quadratic loop, a lost index) fails the PR that causes it.

### D44. Trapezoid and chamfered pads, drill offsets (KiCad pad shapes) (2026-10-14)
Two pad shapes join the model: `trapezoid` (`w`, `h`, `dx`: the left edge is `h + dx` long and the right
`h − dx`, `dy`: the lower (−Y) edge is `w + dy` wide and the upper `w − dy`) and `chamfered_rect` (`w`, `h`,
`chamfer` cut along both edges of each listed corner at 45°, the other corners rounded by `r`), and pads get
an optional `drill_offset`: the hole center relative to the pad (copper) center, before the pad rotation.
Additive, so the schema version stays 1 (older projects read unchanged; only projects using them need this
version). Each shape has one outline definition (`PadShape::outline_path`: vertices, rounded corners as arcs
with their centers) used everywhere: the board copper (exact arcs through `polyclip` curves, so DRC, zone fill,
router obstacles and renders follow), Gerber (a trapezoid is one outline primitive; a chamfered rectangle an
outline through its chamfers and its rounded corners' chords plus a circle on each rounded corner, which
unions to the exact shape; no macro variables, as for the other pads), IPC-2581 (`RectCham` when no corner is
rounded, else a `Contour`; trapezoids as contours), Specctra (polygons). A bottom-side footprint mirrors the
shape (`PadShape::mirrored_x`: `dx` negated, corners left↔right) where outputs draw a pad shape with a board
rotation. Mask openings grow these shapes as polygons (exact offsets with round corners); paste margins too.
Holes everywhere (drill files, IPC-D-356 hole records, X2 drill, hole-to-hole, NPTH zone clearance, router,
3D and STEP drills, renders) are taken from `PlacedPad::hole_at`, the pad's hole center on the board; the
annular ring of an offset hole or of the new shapes is the distance from the hole (its slot axis) to the
outline. KiCad: `rect_delta (dx dy)` in its pad frame (Y down) has `dx` lengthen the left edge and `dy` widen
the edge toward +Y (cadlab's lower edge); chamfers are `chamfer_ratio` × shorter side on the listed corners
of a `roundrect` (rounded others by `roundrect_rratio`) or `rect`; the pad position is the hole and
`(drill … (offset x y))` moves the copper by the offset in the pad frame. These are read from the published
format documentation and checked through the importer's custom-pad path (the hand-written test board puts
each shape next to a custom pad of the same outline on a top and a bottom footprint), not by KiCad's code.
Not done: per-layer pad stacks (KiCad 9 `padstack` modes `front_inner_back` and `custom`): the board copper
is one shape per pad for every layer (`PlacedPad::shape`), so they need a per-layer shape there and in every
consumer (DRC per layer, zone fill per layer, Gerber per layer, DSN padstacks per layer, the export); the
front layer's shape is still kept with `import.pad_approximated`. The corpus manifest was not re-run here
(no `kicad-cli`); its notes did not name these items, so they were left unchanged.
*Why:* trapezoid, chamfered and offset pads are design data that changes copper, masks and drill positions
beyond the comparison tolerances when approximated; one outline definition keeps every output consistent,
and writing them back lets the KiCad DRC oracle check them.

### D45. Agent session ergonomics: CLI shortcuts, argument forms, ERC drivers, silk margins (2026-10-14)
The first scripted agent session (docs/TESTING.md, "Agent sessions"; `tests/agent_session.rs`) drove these
choices. The CLI gains two top-level aliases, `cadlab erc` and `cadlab lint`, and `cadlab drc` alone runs
`drc.run`: they are not new operations (the registry is unchanged, MCP needs none), only the spellings agents try
first, like `undo`/`redo`. JSON-valued flags accept `@file`; schema tuples of strings (points) take `x,y` on the
CLI. Route commands take `budget` with a unit ("60s"), `budget_ms` stays for compatibility, and a bare number
is rejected like a unitless length. Command groups get one-line summaries (`commands::GROUP_SUMMARIES`) shared
by CLI help and MCP tool descriptions. Text summaries print enum values as arguments spell them. Render commands
all take the output path positionally. ERC counts output pins of one component that share a name as one driver
(a USB-C receptacle's two VBUS contacts are one node); outputs of different components still conflict. The
`comfortable` fab margin applies to copper and drill limits only: silkscreen limits are taken as published,
since the fab clips silk at pads and a margin buys no yield, and with it cadlab's own generated footprints
(silk 0.15 mm from pads) failed every fab-derived rule set.
*Why:* each of these cost the agent at least one failed call or a misleading fix (moving parts to cure a
footprint's own silk); the aliases and argument forms make the documented examples true without adding
operations outside the registry.

### D46. DRC and pour parity with KiCad: silkscreen against mask openings, thermal spoke angles and counts, rule pairs, `disallow` and severities (2026-10-14)
Three gaps the open-source corpus listed (TESTING.md, "Known issues") are modeled rather than tolerated.
**Silkscreen** is checked against the solder mask openings by default (`board.rules` `silk_check: mask`):
pads with an opening on that side grown by their mask margin (pad, footprint, `mask_expansion`; NPTH holes
included) and mask drawings of footprints and the board, the latter as a new code
`drc.silk_over_mask_opening`; `silk_check: copper` keeps the former check against pad copper. With no
margin and no drawings both agree, so designs without mask settings report what they did.
**Thermal reliefs** gain a spoke direction and count: pads and footprints may set `thermal_gap`,
`thermal_spoke` and `thermal_angle` (`Overrides`, pad then footprint then zone), zones `thermal_angle` and
`thermal_spokes` (1 to 8, default 4). The angle is relative to the pad's orientation (mirrored on the bottom
side); unset, it is 45° for round pads and 90° for others, which is KiCad's user-visible default (taken from
its documentation and observed behavior, not its code). This changes fills of round pads from axis to
diagonal spokes and turns spokes with rotated pads; axis spokes keep their exact former geometry, slanted
ones are rectangles placed in floating point and rounded to the nanometer, reaching `gap + width` beyond
the farthest crossing of the pad outline. KiCad's file format exposes pad and footprint `thermal_gap`,
`thermal_width` (pads also `thermal_bridge_width`) and pad `thermal_bridge_angle`: all are imported (an
angle equal to the shape default stays unset) and exported (a footprint angle handed to its pads, KiCad
having none at that level). The zone angle and spoke count have no KiCad counterpart and are not
exported. **Custom rules** gain a second scope `with` (KiCad's conditions on `B`: a clearance applies
between an item in `scope` and one in `with`, either way round; resolution keeps "last rule in board
order wins" across single-scope and pair rules), `touches_area` (`intersectsArea`/`insideArea`), `disallow`
(item kinds forbidden in the scope, `drc.disallowed`) and `severity` (error, warning, ignore; KiCad's
`exclusion` read as ignore). A severity only changes how the DRC reports violations the rule decides: the
values still drive fills and the router, and shorts stay errors. A paired rule makes zone fills keep the
largest pair clearance that can involve a zone, so a fill never violates the DRC. Not modeled: terms
relating both items (`A.Net == B.Net`), disallowed `text`, `hole`, `footprint` and via subtypes, track widths
conditioned on `B`; each is reported on import. The corpus manifest is left unchanged: its known
differences for silkscreen and pour connectivity need re-measuring with `kicad-cli`.
*Why:* each gap made the KiCad oracle disagree on real boards for reasons that are design data, not
algorithm: the mask expansion KiCad's silkscreen check sees, the diagonal spokes that reach pads cadlab's
axis spokes missed, and rules the import had to drop. Modeling them once in the shared geometry and
rules keeps every consumer (DRC, fills, exports) consistent, and keeps them editable through commands.

### D47. MCAD exchange completed: generated STEP bodies, IDF outlines and keep-outs, IDX change and response messages (2026-10-14)
**STEP bodies.** The 3D view's package bodies moved to a shared module, `cadlab::body3d`, used by
`render.board3d` and `export.step`. Its solids are now typed: convex polyhedra (`Shape::Faces`) or cylinders along
X or Z (`Shape::Cylinder`), so the STEP writer emits them exactly (planar faces bounded by lines; a cylindrical
face with a seam edge) while the 3D view still draws prisms of the same 20/16 sides (byte-identical where the
shapes did not change). Molded bodies gained chamfered top edges (a ten-face convex solid, in both outputs). Each
solid is its own `MANIFOLD_SOLID_BREP` in the body part, not fused with the others: a boolean union would need
a 3D B-rep kernel for touching boxes, and MCAD tools take a part made of several solids. Polyhedra are checked
before writing (faces on the nanometer grid, outward from the centroid, every edge used once each way); a
solid that collapses is left out, and a part with none falls back to the package box. Attached 3D models were
already faceted B-rep and stay so.
**IDF.** `.ROUTE_OUTLINE`/`.PLACE_OUTLINE` describe the area copper and courtyards keep to (the outer contour
shrunk by `copper_to_edge`, the margin the placer uses); keep-outs map one section per forbidden kind
(`no_tracks`, `no_vias`, `no_footprints` with height 0). IDF 3.0 sections hold one loop with a direction label,
so neither cutouts nor several pieces share a section; routing layers can only say `TOP`, `BOTTOM`, `BOTH`,
`INNER`, `ALL`, so a keep-out on some inner layers says `INNER` (it may overstate, never understate). Pour
keep-outs and rule areas have no section and are left out rather than approximated by a stricter one.
**IDX.** Change messages cover component placements, the part of the collaboration that changes after the
baseline (moves on either side); outline, hole and keep-out edits go in a new baseline. The sender's state is
not stored in the project (provider- and tool-agnostic design data, D12): `export.idx_changes` takes the
messages exchanged so far and rebuilds the item versions from them, so sequences follow the guidelines
(predecessor named explicitly, sequence + 1) and a rejected change in a response falls back to its
predecessor. `place.import_idx` accepts only what it applies (placements of known, unlocked components) and
rejects the rest with the reason, so an `Accept` true always matches the board. Responses are written by
inserting `Accept` and descriptions into the received text instead of re-serializing it, so items cadlab does
not understand pass through unchanged. Reading needs XML: `mcad::xml` is a ~250-line reader (elements,
attributes, text, offsets) rather than a new dependency; it matches local names, so any namespace prefixes
work. The required `Actor` refers to an `EDMDPerson` whose surname is an argument (empty by default).
*Why:* these were the "not yet" items of MANUFACTURING.md that depend only on cadlab; STEP/VRML model import
stays with the oxideav decoders (D36).

### D48. Per-layer pad stacks: footprint-relative layers, one copper item per shape (2026-10-14)
A through-hole pad may carry a `stack` (`PadStack`): its shape on every inner layer (`inner`), on the outer
layer on the other side of the footprint (`back`) and on single inner layers (`layers`, numbered from the
footprint's side); the pad's `shape` is the footprint side's and every layer left out. Layers are relative to
the footprint, like the rest of a footprint, so a library footprint keeps one meaning on either side: on a
bottom-side placement `back` is `F.Cu` and inner layer 1 the deepest. SMD and non-plated pads ignore it.
Additive (schema version unchanged; projects without stacks are written as before). `PlacedPad` keeps its
footprint-side `shape` and lists the layers whose shape differs (`layer_shapes`, `shape_on`), and the board
copper is one item per distinct shape on the layers having it; the items of one pad share `ItemLocal::stack`
and `islands` joins them (the plated hole), so DRC, zone fills (clearances and thermal reliefs per layer),
connectivity, the ratsnest and the router follow without layer-specific code. Gerber copper flashes each
layer's shape, masks the outer shape of their side, IPC-2581 the layer's shape in each copper feature (the
package land pattern keeps the footprint side's), Specctra padstacks one shape per layer (named by the front
shape plus a hash when they differ), the annular ring checks (DRC, fab) the narrowest layer's, the IPC-D-356
record and the footprint preview the footprint side's. `footprint.set` sets or unsets a pad's `stack`.
KiCad 9: `(padstack (mode front_inner_back|custom) (layer "Inner"|"In<k>.Cu"|"B.Cu" (shape …) (size …) …))`
with the pad's own shape being `F.Cu`'s; stacks are keyed by board layers (a flipped footprint's stack
flipped), so the importer takes `B.Cu`'s shape as a bottom-side pad's own and reverses inner layers, and the
writer does the inverse (`front_inner_back` unless single inner layers differ, then `custom` with every inner
layer and `B.Cu`). A layer `offset` other than the pad's turns that layer's shape into a polygon around the
pad's center; per-layer thermal and zone settings are not modeled (the pad's apply). Read from the published
format documentation and files KiCad writes, not from its code; not checked against `kicad-cli` here (none in
this environment), and the corpus was not re-run.
*Why:* per-layer shapes are design data (smaller inner pads for clearance between inner traces, different
back pads) that changed clearances, fills and outputs when the front shape was copied to every layer.
Splitting a stacked pad into per-shape copper items reuses every item-based consumer unchanged, and pads
without a stack keep exactly their former single item, so every existing output is byte-identical.

### D49. Impedance field solver: 2D boundary elements with plane Green's functions, closed forms stay the default (2026-10-14)
`impedance.calc` / `impedance.solve` gain `method: closed_form | field_solver`. The field solver
(`electrical::fieldsolver`) is a quasi-static boundary element method (method of moments): piecewise-constant
total charge on conductor surfaces and dielectric interfaces, point matching at panel midpoints (conductor
potential; continuity of normal D), in a vacuum Green's function that already satisfies the ground planes
(one image for microstrip, the closed-form parallel-plate function for stripline), so planes are never
discretized. Log-kernel panel integrals are analytic, the parallel-plate remainder uses Gauss quadrature.
Panels grade towards conductor corners; every structure is symmetric about the line centre, so half is
solved with even/odd mirror kernels; the dense system is solved by LU in a fixed order (deterministic,
single-threaded). From C and C₀: Z₀, εeff, C, L, and for pairs Zdiff / Zcomm from the mixed-mode transform of
Z = C⁻¹(CL)^½ (odd/even for symmetric pairs, also defined for the asymmetric broadside microstrip pair).
Modeled: microstrip, embedded microstrip, stripline (a stackup stripline keeps its two dielectrics apart),
edge-coupled and broadside pairs (broadside: the layer and the next one down), grounded coplanar ground,
conformal solder mask, trapezoidal etch. A field-solver-only option (`etch`, `mask`, `coplanar_gap`,
`broadside`, `er2`) selects the field solver when `method` is unset, and is refused with an explicit
`closed_form` (`impedance.needs_field_solver`). Default mask εr 3.8 (typical LPI; noted as assumed). The DRC
keeps the closed forms. Losses are not computed (added by D52).
*Why:* boundary elements fit layered PCB sections best: unknowns live only on boundaries (a few hundred
instead of tens of thousands of grid nodes), open regions need no artificial box, edge singularities are
resolved by grading panels rather than refining a 2D grid, and exact panel integrals give 0.02–0.1 % against
the exact (Cohn) stripline results at a few milliseconds per line; a finite-difference SOR grid would need
nonuniform meshing, an outer boundary and many iterations for the same accuracy, and staircases trapezoids.
Plane Green's functions avoid truncating the planes, the largest source of error in free-space BEM. The
closed forms stay the default: they are instant, exactly reproducible across versions, already within a
few percent in their ranges, and the DRC evaluates them per track; the solver is one option away, and
picked automatically when the closed forms cannot model the request.

### D50. Agent session 2: spread automatic placement, unrouted summary, KiCad footprint names, `footprint.create` (2026-10-14)
The items session 1 left open (docs/TESTING.md, "Agent sessions") are fixed, and the session replays with them.
**Placement:** `place.auto` (groups) now spreads groups over the board by default: each anchor's room is its
group's area times the free area's share (rooms taking `SPREAD_DENSITY` = 60 % of the free area in all, at most
2.5 times the group), the largest IC goes first to the center even when other parts are locked, connectors
are penalized per pad for each other connector closer than a quarter of the board's width plus height, small
ICs whose signal nets reach one larger IC only are its satellites (inside its room), and improvement keeps
anchors in their room; part boxes then also cover each footprint's silkscreen (generated pin-1 dots sit
outside the IPC courtyard and landed on neighbours' pads once groups moved). `compact: true` is the former
placement exactly, and the router benchmark boards use it, so `tests/golden/bench/route.json` stays the
router's baseline rather than the placer's: with spreading the same boards route at 100 % with more track
(ATtiny 104 → 136 mm; STM32 2-layer 57 → 84 vias), the expected cost of using the board. **DRC:** `drc.run` takes `unrouted` (`auto` default: more than 10 unrouted
connections become one `drc.unrouted` with the count per net and the nets as subjects; `each`; `summary`) and
reports the total in `unrouted`; the library's `drc::check` still returns one finding per connection (the router
and the KiCad cross-checks use them). **describe:** text output lists enum values of fields nested in objects
(`pins[].kind: ...`); the compact type label is unchanged. **KiCad footprint import:** a footprint is named after
the name inside the file (`(footprint "NAME"`), the file name only when there is none; an info
`import.name_from_file` notes a difference; `footprint` filters match either name; `footprint.import_kicad`
gains `name` to choose the project name (one footprint); symbols' `Footprint` fields, which use file names
like KiCad's library table, are resolved through the imported items' KiCad names. Footprints imported
earlier keep their names; re-importing such a file adds the internally named footprint next to them.
**`footprint.create`:** a footprint from a pad list (number, at, size, shape, radius, rotation, drill or slot,
layers `top|bottom|all|npth`, rows by `count` + `pitch`), optional body centered on the origin, built by
`landpattern::from_pads` with the generator's silkscreen, pin-1, fab and courtyard drawing.
*Why:* an agent's first placement should look like a board, not a cluster, while keeping what made the packed
placement route well (passives at their pins, connectors at edges); a list of 59 identical errors hides the
one real problem in a pre-route check; the footprint name an agent reads in the file is the one it types
next; and land patterns are datasheet tables of pads, which a command should take directly, rather than a
detour through KiCad's file format.

### D51. Oracle refresh for KiCad 10.0.7: silkscreen minimum to copper, trimmed thermal spokes, corpus re-measured (2026-10-14)
The CI's KiCad PPA moved from 10.0.6 to 10.0.7, and D44–D48 had left the corpus manifest unmeasured.
**KiCad 10.0.7** no longer reports a pad/track clearance on a track it already reports crossing a track of
the pad's net (the cross-check `short` board: identical files, 10.0.6 reported the 0.156 mm gap to U1.5);
the gap is a real violation, so cadlab keeps it and the cross-check allows it on that board for KiCad ≥ 10.0.7
only (the test reads `kicad-cli version`), so the entry is used on either version.
**Silkscreen** (`silk_check: mask`): `silk_to_pad` is measured to pad copper, and mask openings (pads grown
by their margin, non-plated holes, mask drawings) only forbid overlaps. D46 added the minimum on top of the
grown opening, counting the margin twice: the cross-check's `local_settings` board (silk 0.15 mm from copper,
0.12 mm from a 0.03 mm-grown opening, `min_silk_clearance` 0.15 mm) became a cadlab-only warning. A new
cross-check board (`silk_near_pad`, a line 0.08 mm from a pad) shows KiCad's `silk_over_copper` applies no
clearance at all (overlaps only; its silk clearance is silkscreen to silkscreen): cadlab keeps its fab
minimum to copper, allowed on that board with that reason. The corpus also shows KiCad's silkscreen check
does not grow pads by the board-level `pad_to_mask_clearance` (corne-cherry 0.2 mm, cynthion 0.05 mm), while
it does by pad and footprint margins (glasgow-revC3's fiducials); the fab grows them all, so cadlab does too
and those boards record the difference.
**Thermal spokes:** D46's diagonal spokes on round pads squeezed between other-net pads crossed a clearance
somewhere along their `gap + width` overshoot and were all dropped, where KiCad trims them and the axis
spokes used to fit (new unrouted pads on corne-cherry and glasgow-revD1). A spoke that does not fit whole is
now trimmed to the allowed area, kept within the pad's gap and the pour, and only if it joins the pour over at
least half its width (no slivers). This also connects pads the earlier fill missed (corpus `drc.unrouted`
known entries shrank or went away).
**Import UUID table:** drawings of pad-less footprints (imported as board graphics) map to their `graphic#`,
and KiCad 6/7 `tstamp`s of pads and footprint drawings are read like `uuid`s, so KiCad findings on them pair
with cadlab's (silkscreen over a logo's NPTH neighbour on glasgow-revC3); the cross-check matching names an
unnumbered pad by its footprint, as both UUID tables do.
*Why:* the oracle is only useful if each difference is either fixed or explained by a measured cause; the
reports, not assumptions, decided which side to change (the silkscreen double margin and the dropped spokes
were cadlab regressions; the clearance after a crossing and the board-level mask expansion are KiCad's
behavior).

### D52. Transmission line losses: loss tangent in the stackup, skin effect by perturbation, roughness factors, `net.loss` (2026-10-15)
Stackup dielectrics gain an optional `loss_tangent` (exact decimal, written plainly as `"0.02"`; unset:
0.02 assumed with a warning, like εr 4.5), set with `board.dielectric --loss-tangent`. `impedance.calc` takes
`freq` (one or more) or a logarithmic sweep (`freq_from`, `freq_to`, `freq_points`) and reports per frequency
R, L, G, C per meter, the complex Z₀, the attenuation in dB/m and dB/in with its conductor and dielectric
parts, and with the field solver the differential attenuation of a pair; `net.loss` multiplies the
single-ended attenuation of a routed net's tracks (per layer and width) by their length. **Dielectric:**
first-order perturbation with complex permittivities, G = ω Σ tan δ · ε ∂C/∂ε; the field solver
differentiates its C per group of regions sharing a loss tangent (two extra solves per group), the closed
forms differentiate their εeff in εr (Pozar's linear filling factor is the same to ~1 %). **Conductor:**
skin-effect surface resistance times a geometry factor R/Rs: the closed forms apply Wheeler's incremental
inductance rule to their own Z₀ (finite difference of the receded geometry), the field solver integrates
∮|Jₛ|² of the quasi-TEM current, which is c times the vacuum surface charge, on traces, coplanar grounds and,
in closed form from the plane Green's functions, on the planes; corners are graded 1000× finer for it (the
singular current converges as size^(1/3)). Roughness is a multiplier (Hammerstad–Jensen from an RMS value, or
Huray's snowball model from radius and surface ratio) on every surface; internal inductance Rs/ω per unit R/Rs
is added; R never falls below the trace's DC resistance, and a note says where δ exceeds half the copper.
Conductivity defaults to 58 MS/m (IEC 60028). Results are rounded to 4 significant digits; `Unit::Siemens`
was added for G and the conductivity.
*Why:* the perturbation forms are the textbook methods (Pozar, Wheeler, Harrington-style BEM), need nothing
beyond the quasi-static solutions already computed, and are accurate where PCB losses matter (above tens of
MHz, δ well below the copper); roughness and the laminate's tan δ dominate real losses, so they are explicit
inputs with stated defaults rather than hidden constants. The current method was preferred to Wheeler's rule
on the BEM because finite differences of a re-meshed geometry are noisy, while the charge densities are already
there. Frequency-independent εr and tan δ (no Djordjevic–Sarkar model) keep the stored data a pair of decimals
per dielectric; the user gives the datasheet values at the frequency of interest. A net's loss ignores vias,
discontinuities and pair coupling: it is a budget figure, not an S-parameter simulation (left to the 3D solver
stretch goal).

### D53. Agent session 3: offset footprint bodies, placement hints and density, fab stackups, buck-aware ERC/lint, pair-aware placement (2026-10-15)
Session 2's open items, and what session 3 (STM32 USB device on four layers, docs/TESTING.md) got stuck on.
**Footprint bodies:** `Body` gains `at`, the body center in footprint coordinates (default the origin, not
serialized then), used by the generated fab outline and silkscreen, the 3D view's fallback body, STEP, IDF and
IDX; `footprint.create` takes it in `body` and an `origin` (`given` default, `body`, `pads`) that shifts pads
and body together. Pads stay in datasheet coordinates; the pin-1 dot goes above or below pad 1 when another
pad (a shell) is left of it. **Placement hints:** `place.hint` stores per-designator hints in `board.json`
(`placement_hints`, only `edge` for now: `any|left|right|top|bottom`), which every `place.auto` applies: the
part goes to that edge and only moves along it or closer, a two-terminal part with a hint is placed like an
anchor, identical footprints with different hints are not swapped; hints follow renames and removals. Hints
are data rather than `place.auto` flags so a re-placement (`--replace`) keeps them. `place.auto --density`
(percent of the free area the rooms take, default 60) is an option only. **Spread placement only** (so that
`compact`, the router benchmarks' placement, stays D50's exactly): receptacles face their edge (the side the
courtyard center lies from the pads' centroid; those rotations first at every room size), passives keep out of
the breakout boxes of differential pair ends, a rail's first capacitor goes to a regulator input, and
passives without a signal net go to the smallest shared power net (a buck's inductor to its switch pin).
**Fab stackups:** fab profiles may list `[[process.stackup]]` (id, thickness, copper, dielectrics with εr and
material), JLCPCB's three 1.6 mm four-layer impedance stackups from its impedance page; `board.fab_stackup`
copies one into the board (values only: the design names no fab), `fab.check` accepts their copper (the
stackup page's 15.2 µm inner copper vs the capability page's 0.5 oz), and `fab.export` records in
`fab-lock.json` (`process.stackup`) and its summary the published stackup the board matches exactly, which is
what has to be ordered. **ERC/lint:** a net reached from a powered net through a two-pin inductor, ferrite bead
or fuse is powered (a buck's output, a filtered analog rail); a regulator output driving an inductor has its
output capacitor checked behind it, not on the switch node. **Parts:** `part.create --replace` redefines a part
in place (refused when a connected pin would disappear); unmapped shell/shield/mounting pads get one
`part.unmapped_shield` warning with the pin (and pin map) to add. **Router:** interleaved contacts of one part
(USB-C's two rows) are left to the ordinary router like partner-less pads instead of failing as a
flow-through link. **CLI:** a bare word given to a JSON-valued flag is a JSON string (`--outline board`).
*Why:* the agent typed datasheet coordinates and wanted to say "this connector at the bottom, LEDs on top"
once; ordering an impedance-controlled board needs the fab's stackup both in the design (for the solver) and
in the order (the lock), with the numbers coming from the fab's published data rather than the agent's memory;
and a buck converter, a USB-C receptacle and a D+ pull-up are ordinary parts of a USB device that each tripped a
rule written for the simpler case.

### D54. openEMS full-wave export: CSXCAD geometry plus a Python driver, Touchstone reader to read results back (2026-10-15)
First step of the full-wave stretch goal. `export.openems` writes a board region (from `nets`, `around` items
or a `window`, plus a margin) as two files: `<name>.xml`, an `<openEMS>` document holding only the CSXCAD
`<ContinuousStructure>` (one `Material` per dielectric with εr and, from the new optional
`Dielectric::loss_tangent` (shared with the loss calculations of D52, 0.02 when unset), a conductivity κ = 2π f₀ ε₀ εr tan δ at the excitation's center; one perfect-
conductor `Metal` per net with each layer's copper union clipped to the region, extruded through the copper
thickness or as zero-thickness sheets; vias and plated holes as metal cylinders; non-plated holes as air) and
its `<RectilinearGrid>`; and `<name>.py`, a driver that reads that geometry and adds what belongs to a run:
Gaussian excitation (f_min .. f_max), PML or MUR boundaries, and lumped (`AddLumpedPort`, resistive sheet from
the reference copper to the signal copper) or microstrip (`AddMSLPort`) ports, one run per excited port, S-
parameters from `CalcPort` written as Touchstone (1.1, or 2.0 when port impedances differ). The mesh is ours:
fixed lines at metal edges, layer surfaces and ports (close metal lines merged), gaps filled with cells of at
most λ/N in the stack's highest εr (λ₀/N in the air), four cells per dielectric, neighbours within a grading
ratio. Polygons with holes are cut into hole-free pieces by vertical lines through each hole (CSXCAD polygons
have no holes). Coordinates are written exactly in mm from the integer nanometers; output is deterministic
(golden files). `src/touchstone.rs` reads (and writes) Touchstone 1.1 and 2.0 per the IBIS Open Forum
specifications, converts Y/Z to S, and `touchstone.summary` reports |S| (dB), phase at frequencies and band
extremes. openEMS is an optional oracle (`Oracle::OpenEms`): when installed, the test runs the driver.
*Why:* openEMS is GPL, so only its documented formats and Python interface are used (no code read, linked or
bundled), and it stays an external tool. Ports, excitation and boundaries go in the driver rather than the
XML because openEMS's port post-processing (incident/reflected waves from voltage and current probes,
microstrip feed and measurement planes) lives in its Python/Octave port classes: writing raw probes into the
XML would mean re-implementing them from undocumented internals, while the documented calls are stable and
are how openEMS's own tutorials build ports; the geometry and mesh, which are cadlab's knowledge of the board,
stay in a file other openEMS tools (AppCSXCAD) can open. Python rather than Octave: it is openEMS's maintained
interface, needs no licence and writes the Touchstone file with numpy alone. Perfect conductors and a
loss tangent turned into a conductivity at one frequency keep the model small and standard; copper
conductivity, roughness and solder mask are documented as not modeled. A Touchstone reader closes the loop
for agents: they run the driver and read the numbers back through a command, with no plotting tool.

### D55. Pair and length-group lengths along the signal path (stubs apart); spread placement stretches and relaxes groups; fab stackups listed (2026-10-15)
Session 3's open items (docs/TESTING.md). **Signal paths:** D38 measured a net's length over all of its copper,
so a D+ pull-up's branch and a USB-C receptacle's row link counted as length: the pair showed 6 mm of "skew",
and `route.tune` added 30 skew bumps that left most of the pair uncoupled. A pair's nets and a length group's
members are now measured along their signal path, the shortest path through the net's copper graph (track ends
and T-junctions per layer, tracks, via spans through the stackup, pads touching copper) between the pads of two
end components. Ends are data (`DiffPair`/`LengthGroup` `from`, `to`; optional, no migration), set with the new
`diffpair.set NAME --from J1 --to U3` (and on `diffpair.add`, `lengthgroup.set`; `auto` clears), validated
against the nets' pins, renamed and dropped with their component; by default a pair's ends are the two
components with pads on both nets farthest apart along the routing (a pull-up has a pad on one net only), a
net's the two farthest of all its components. The rest of the net is reported as stubs (length, attach point,
pads reached; `diffpair.list` `path_p`/`path_n`), with DRC warnings `drc.diffpair_stub` / `drc.length_stub`
above `max_stub` (default 5 mm, the usual high-speed USB guidance for stubs). Skew, coupled and uncoupled length,
group lengths, `route.tune` (only path tracks are meandered or bumped) and the DRC all use the path; without a
connected path the length falls back to all copper as before. **Spread placement:** after the constructive
placement, groups move as rigid blocks: a stretch of the arrangement where it leaves an empty band wider than
an eighth of the board (relative places kept, so groups stay apart and the ratsnest's topology holds), then a
relaxation (1–8 mm shifts and mirror images across the central IC) minimizing the ratsnest plus a weight times
the board's unevenness on two staggered window grids; the largest stretch whose ratsnest grows at most 25 %
(scaled by `density`) is kept, else none. The central IC and the ends of differential pairs do not move then nor
in the improvement that follows (`compact` is unchanged; the router benchmarks use it, so their baseline holds).
**`board.fab_stackup FAB`** without a stackup name lists the fab's stackups (values and the command applying
each) instead of failing with `board.stackup_choose`.
*Why:* the pair's length that matters is the one the signal travels between driver and receiver; a stub is a
different defect (a reflection), so it is reported and limited on its own rather than "compensated" by
uncoupling the pair. Placement must keep what routes well (the pair's ends where their breakouts were planned,
groups in the arrangement connectivity chose) while using an outline the designer chose larger than the parts;
stretching the arrangement keeps its topology, and a ratsnest budget bounds what using the board may cost. A
question an agent asks ("which stackups?") should get an answer, not an error.

### D56. Corpus residuals: exact clearance gaps, KiCad-measured tolerances, courtyard pieces and sides, copper-to-hole clearance, pad stacks in every output (2026-10-15)
The corpus entries D51 left "not analyzed" were each traced to a cause; every cause but the stroke font was
a cadlab gap, fixed here. Tolerances were measured with `kicad-cli` 10.0.7 on cross-check boards built for
the purpose (probes run in the oracle job), not taken from KiCad's code.
**Exact gaps.** A clearance finding between items with an exact outline (straight tracks, vias, pads other
than chamfered rectangles with rounded corners: a point, segment or polygon grown by a radius,
`board::exact`) is measured again on it; the polygons' outward arc approximation had added up to 2 µm to
real deficits, so the documented 2 µm tolerance was sometimes none. corne-cherry's three "0.188 mm" gaps
were 0.1895 mm (0.499 µm short of 0.19 mm). KiCad's own tolerance is 0.5 µm (a clearance 0.55 µm short is
reported, 0.50 µm passes); cadlab keeps 2 µm because its router keeps clearances to within 1 µm
(`router::index::TOL`; a 0.5 µm DRC tolerance made the router benchmark rip up two connections), so
deficits between 0.5 and 2 µm are KiCad's alone, below any fab's resolution.
**Courtyards.** KiCad reports courtyard overlaps from 10 µm (9.95 µm passes): overlaps narrower than
`COURTYARD_TOLERANCE` = 10 µm are touching, which removed glasgow-revD1's 13 and tinytapeout-demo's 3
library-slop overlaps (2.5–9.3 µm). A footprint's courtyard is now every closed courtyard drawing: on its
side the largest piece is the courtyard and further pieces stay closed courtyard drawings, and drawings
on the other side stay too, with `back` (a key switch's body above its hot-swap socket footprint on the
bottom, corne-cherry). Before, several pieces became their bounding box (tinytapeout-demo's carrier J5,
two rectangles 35 mm apart, then "overlapped" 30 parts and pulled tracks into a neck-down rule), and the
other side's drawings were dropped (a 0.25 mm box around the pads was generated on the wrong side). The
courtyard DRC, holes in courtyards, keep-outs and the export use every piece; `intersectsCourtyard` rules
(`in_courtyard`) the pieces on the footprint's side. An item is in a courtyard or area when it reaches
more than the 2 µm tolerance in, measured on its exact outline when it has one (KiCad takes a track whose
straight side reaches 1 µm in; a round track end 1.4 µm in is outside in both tools, and one 5.0 µm in is
in for cadlab but not for KiCad, whose arc approximation of the end lies inside it: tinytapeout-demo's
remaining track width difference).
**Rule areas sharing a name** (KiCad's `enclosedByArea('X')` then means any of them) keep unique names
(`X`, `X_2`), and a custom rule naming one becomes one rule per area, in place (tinytapeout-demo's second
`CARRIERBREAKOUT`, whose tracks lost their 0.16 mm clearance).
**Designators** that cadlab cannot use but that are annotated (`POWER SW`) are renamed to their letters and
the next free number (`POWERSW1`, `import.refdes_renamed`) when the circuit is built from the board, so
their copper and nets are kept (sweep-v2.2's power switches: paste and drill now match); unannotated ones
(`REF**`) are left out as before; with a circuit, the schematic decides.
**Copper-to-hole clearance.** KiCad's pour keeps its copper-to-hole clearance (`min_hole_clearance`,
0.25 mm on corne-cherry) from a copper-less NPTH, where cadlab kept the zone clearance (0.508 mm); a spoke
reaching KiCad's pour found none in cadlab's. Decided by what the fab needs: a non-plated hole is no
copper, so the electrical clearance between nets does not apply to it; what fabs specify is "NPTH to
copper" (drill wander and the hole tolerance must not reach copper; Aisler 0.25 mm, NextPCB 8 mil in the
fab profiles' notes), which is KiCad's documented copper-to-hole clearance. `board.rules` gains
`hole_clearance` (unset by default: fills keep their own clearance from non-plated holes as before, the
conservative choice for designs that never stated one); set, fills keep it from non-plated holes (and
from plated holes of other nets), and the DRC checks copper to every hole it does not belong to
(`drc.hole_clearance`: any copper to a non-plated hole, a mounting hole's own pads included; copper of
other nets to a plated one, except net-tied copper and copper touching the pad, which is a short). The
KiCad import reads `min_hole_clearance`, the export writes it (0 when unset: KiCad then checks none
either), and the cross-check pairs KiCad's `hole_clearance` with it (glasgow-revD1's mounting hole H5,
whose pad covers its own hole, in both tools). corne-cherry's pours now reach every pad KiCad's reach.
**Pad stacks in every output** (D48's remaining consumers): IPC-D-356A writes a through-hole pad whose
outer lands differ as one `317` record per side (`A01` with the top land, `ANN` with the bottom one), the
record layout's access code being the only per-side field; IPC-2581 writes a `PadStackDef` per stacked
pad of a package (hole and each copper layer's land in the package's frame, before `Datum` as the
revision C schema orders the step) and the land pattern's pad refers to it; the footprint preview draws
the other side's land under the footprint side's (blue) and inner lands as outlines (it also draws pads
rotated now). A stack on an SMD pad gives the pad the stack's shape on its copper layer (no longer
`import.pad_approximated`).
**Smaller fixes.** Exported board holes hide their name (cadlab's legend prints none), and a footprint
whose only courtyard is on its other side places its reference above that one; a board-only footprint
with one plated hole on a net becomes a via only when its pad is tented (sweep-v2.2's exposed battery
contacts `B+`/`B-` stay plated board holes with their mask openings); copper rings around non-plated holes,
which cadlab does not model, are reported (`import.npth_copper`); the corpus comparison pairs open
connections that share an end (an island's end is either tool's choice).
**Not done: the stroke font.** Texts on copper differ in the Gerber copper layers and copper-to-edge
(cynthion, lumenpnp-mobo) because cadlab draws the Hershey simplex font (public domain) and KiCad its own
stroke font, whose glyph data lives only in KiCad's sources; matching it would mean copying KiCad's data,
which the project rules exclude, and no published specification of its metrics exists. Recorded.
*Why:* each residual was a measured cause, and only tolerances were taken from KiCad's observed behavior,
where KiCad's choice is also the fab's (sub-micron clearance noise, courtyard drawing slop); the
copper-to-hole rule follows the fab's specification rather than KiCad's, which happen to agree.

### D57. Router corpus completion: grid snapped to fine pad pitches, USB-C crossover vias, failing searches skipped (2026-10-15)
Profiling the corpus re-route (`route_bench --corpus`, phase timeline with `CADLAB_BENCH_TRACE`) showed that
the remaining failures had three causes the router could address without new rules. **Walled-in QFN pads:**
the grid pitch, (width + clearance) / 2 = 0.195 mm on corne-cherry, drifts against a 0.4 mm pad pitch, so
somewhere along each row two neighbors map to grid lines one apart and every other pad found no escape (41 of 53
and 37 of 52). When the pad pitch most fine-pitch rows share (8 pads or more, multi-pin parts) is `2m` times a
pitch at most 6 % coarser than the computed one, the grid takes that pitch: every pad of such a row sits at the
same offset, and escapes stay a pad pitch apart. Neck-down widths also keep the grid's sampling margin and the
router's tolerances, so two necked escapes beside each other are compatible as the grid samples them.
**Crossovers:** in a row of fine-pitch pads where two nets interleave tightly (`X Y X Y`, USB-C D+/D−), the
inner pad of one net gets a via straight out of the pad, the shallowest that leaves both neighbors an exit
(straight, then at 45° away from it), and those exits are reserved; the net's other pad gets its own exit and via
only when a neighbor's exit leaves the router no grid access to it (always adding it made the small nrfmicro
board worse, with the D+/D− connections walled in by the extra copper). The router then joins one net around
the via and the other on another layer, as such receptacles are routed by hand; unused fanout is dropped as
before. **Budget:** failing searches visited everything reachable first (on glasgow-revD1, 38 of the first
iteration's 52 million node visits and 44 of legalization's 46 million). Two exact tests skip them: static
reachability (union-find components per rule profile over the points every net may use, merged per net with
the points only it may use; negotiation passes other nets' routing, so separated ends have no path) and, for
searches that may not share, a backward visit from the targets through the static maps and around other nets'
routing, up to 50 000 nodes, that proves them walled in. Both only ever join more than the search reaches (no
bend limit, no own-via spacing), so no result changes; the generated-board benchmark is identical. The
present-congestion factor is capped at 1000 (VPR's `max_pres_fac`): beyond it, the searches of the few nets
still in conflict at the end of a long negotiation only visit more of the board. The A* queue became a 4-ary
heap of (cost, node) keys, popping in exactly the order of the binary heap it replaces.
Tried and dropped: a weighted heuristic in negotiation (1.2–1.5) and a search region around each connection (fewer
node visits only where the searches flood, no completion gain), stopping negotiation at fewer iterations or on
stagnation (completion fell on corne-cherry and glasgow-revC3: the iterations help), a slower present-factor
schedule or a larger history factor (worse on nrfmicro), and windows of nets routed in their sequential order
with speculative parallel searches validated by the area each search read (exactly sequential results for any
thread count, but negotiation converged far slower than with today's batches of nets with disjoint regions, 3–8×
the time on the generated boards). Scoped custom rules (tinytapeout-demo's 0.16 mm clearance inside its QFN-80
courtyard, which most of its remaining failures need) are not applied by the router yet.
*Why:* the corpus failures were measured, not guessed: the walled-in pads were a sampling artifact, the USB-C
crossover a topological constraint that one via resolves, and the budget went to searches whose failure can be
proven cheaply; each change is exact or checked exactly, so DRC stays clean and the benchmark baseline did not
move. corne-cherry 91.8 % → 99.1 %, glasgow-revD1 92.5 % → 93.7 %, tinytapeout-demo 81.1 % → 83.2 % (ROUTER.md).

### D58. MCP agent session 4: hints with JSON arguments, structured server errors, DRC filters and waivers, KiCad migration fixes (2026-10-15)
Session 4 (docs/TESTING.md) migrated a KiCad project through the MCP server only. What it changed:
**MCP surface.** Hints stay written once, CLI-style (`net.set +5V --driven`): the server rewrites every
backticked command span of a hint into `group.action {json args}` by parsing it with the CLI's own clap
mapping (positionals, flags, lists, points), so an MCP client can call a hint as written and the CLI keeps
its form; spans that do not parse stay. `history` and `drc` run their default action without `action`, as on the
CLI; `action` may be `group.action`. Errors of the server itself carry `kind`, `code`, `message` and `hint` like
command errors (`mcp.missing_action` lists the actions; `describe` of an unknown name suggests one and
explains the meta tools); unknown tools get a "did you mean" in the JSON-RPC error. `describe` leaves the output
schema out unless `output_schema: true` (it was most of the 8–16 KB per command) and names the tool and action;
every tool property has a description. Argument type errors locate the argument in the schema (`at[0]`) and
point at units, since serde's message does not say where.
**DRC.** `drc.run` takes `severity`, `codes` (exact or prefix), `limit`, `offset`, and reports `by_code`,
`listed` and `not_listed`; counts never depend on the selection, and the CLI exits 3 on errors even when
they are not listed. Findings can be **waived** (`drc.waive` code + subjects + reason, `drc.unwaive`,
`drc.waivers`), stored in `board.json` `drc_waivers`: a waived finding leaves the errors and warnings and is
counted; unused waivers are reported; fab checks drop waived warnings but never fab errors. A waiver is a
recorded decision with a reason, which KiCad projects express with DRC exclusions (not imported: their
format keys on KiCad item UUIDs and positions). `drc.footprint_outside` says how far the courtyard
overhangs; `zone.split` says where the smaller islands are.
**Migration.** Filled silkscreen polygons of pad-less footprints (kibuzzard labels, logos) stay filled
(they became zero-width outlines that printed nothing and raised 76 width warnings). Symbols without pins
(fiducials) get their placeholder pin marked no-connect, and a footprint without numbered pads does not
report missing pads. Placeholder warnings are one per part, and they are dropped once the board import gave
the part its footprint. "Part #"-style fields are MPNs. `bom.replace` and `part.set footprint` drop the
placement's own footprint choice (an imported board's), which had kept the old package on the board, and
warn (`board.footprint_changed_routed`) when routed nets met the old pads, naming the nets to rip up.
Placement (`place.auto`, `place.near`) keeps new parts clear of the tracks and vias already on the board.
`render` highlights accept subjects as findings print them (`net:GND`, `U1.3`).
*Why:* an MCP client has no shell to translate CLI syntax, no `grep` for a 43 KB DRC report, and no way to
accept an intended violation; each fix removes a step where the session agent guessed (and guessed wrong:
`{"net": "+5V"}` for `nets`) or had to fight output size.

### D59. Built-in full-wave solver: FDTD on the openEMS export's region and mesh, `sim.sparams` (2026-10-15)
The 3D solver of the full-wave stretch goal is a finite-difference time-domain engine (`src/fdtd/`), written
from Yee (1966) and Taflove & Hagness (*Computational Electrodynamics*, 3rd ed.: the integral form on graded
grids, CPML after Roden & Gedney 2000, lumped elements after Piket-May, Taflove & Baron 1994); no code of
openEMS, Meep, gprMax or other solvers was read. It reuses `openems::extract` (split out of `export.openems`):
region, stack, copper, holes, ports and mesh are the export's, so the two describe the same model and the
export can cross-check the solver. Voxelization: dielectrics per cell inside the board outline (σ = 2π f₀ ε₀ εr
tan δ, as the export), E-edge coefficients from the area-weighted average of the four surrounding cells,
perfect-conductor edges inside copper volumes or on sheets, vias as cylinders plus their nearest node column.
CPML in the outer `pml_cells` cells (polynomial m = 3, κ = 1, small CFS α), PEC walls behind. Lumped ports are
resistive voltage sources (the resistance folded into the edge conductivity, sampled exactly: V + z0 I = V_s);
microstrip ports feed the same source on the line and measure V (line integral) and I (H circulation on both
sides of the plane, interpolated) at their reference plane, which gets mesh lines (`port_planes`, off for the
export). S = B A⁻¹ from the DFT power waves of all ports over all runs (one per port), written with the existing
Touchstone writer. The thirds rule becomes an export option (`thirds`), on by default in `sim.sparams`: without
it an FDTD strip is about a third of a cell too wide (Z₀ 3–10 % low in the tests), with it within 1 % at four
cells across. Fields are f32, coefficients and DFTs f64; planes are split between threads of a pool sized one
per 131072 nodes (waking 64 threads per update cost more than a small grid's update), every update reading
only the other field and reductions summed per plane in order: bit-identical for any thread count. Guards:
`max_cells` (4 M), `timesteps` per run (100 k), the energy end criterion (40 dB); progress every 32 steps and
cancellation through the command context, like the router. Validated (tests/fdtd.rs) against the 2D field
solver, the D52 loss model, cavity resonances and Q, energy conservation and the Courant limit; heavier cases
are `#[ignore]` tests run in release.
*Why:* FDTD rather than FEM because the openEMS export already produced exactly what FDTD needs (a graded
rectilinear mesh with lines at metal edges and layer surfaces), one time-domain run covers the whole band per
port, the update is explicit and needs no sparse solver or dependency, and its accuracy is understood from
the textbooks (Courant limit, dispersion, staircasing). Reusing the extraction keeps one notion of "region"
for agents and lets openEMS validate the built-in solver. A constant σ instead of a dispersive model keeps the
stored data two decimals per dielectric (D52) and matches the export; perfect conductors keep the model
small, conductor loss staying the per-length budget of D52. S = B A⁻¹ instead of b/a because a microstrip
port's own source is not matched to the line it feeds; f32 fields halve memory and time, with sums kept in f64.

### D60. Pour residuals: copper rings around non-plated holes, spokes along rows of same-net pads, open connections paired by island; DRC tolerance kept (2026-10-15)
**Copper around non-plated holes.** KiCad `np_thru_hole` pads larger than their hole (sweep-v2.2's power
switch pegs: 1 mm copper around 0.9 mm drills) lost their copper (`import.npth_copper`, two Gerber layers
known to differ). `PadKind::Npth` gains `copper` (default false, so existing projects read unchanged): the
pad's shape is then an unplated ring on every copper layer, on no net (a numbered one too: nothing plates it
to a pin), one copper item per layer so nothing joins the layers through the hole. Every consumer follows from
the placed pad: DRC clearances to every net (its own hole is not a hole clearance), fills keep clear, the
router sees an obstacle, Gerber flashes it with the X2 aperture function `WasherPad` (the specification's
pad around a non-plated hole without electrical function), the hole stays in the NPTH drill file,
IPC-D-356A writes its `367` record with the ring's size, the DSN a keep-out circle around it, the KiCad
export an `np_thru_hole` pad of its shape (offset kept); `footprint.create` takes `layers: npth_copper`.
KiCad's layer list is not kept: copper on some copper layers only goes on all of them
(`import.pad_approximated`). The sweep-v2.2 copper layers now match without pours.
**Spokes along a row of pads.** buspirate5-rev10's four open GND connections were a fill gap: rendering both
fills around them (KiCad's stored fill, cadlab's `render board --around`) showed KiCad keeping the spokes that
run from one GND pad into the next along a resistor array or a pair of 0402s, where every pad lies in its
neighbours' thermal gaps and the spokes across the row are blocked; cadlab dropped them for not reaching the
pour. A whole spoke (in the allowed area) that reaches another thermal pad of the zone's net is now kept once
either pad is joined to the pour by a spoke of its own or through such spokes (fixed point, in pad order, so
deterministic). Spokes that join only pads cut off from the pour are still dropped (no copper the pour does
not reach).
**Open connections paired by island.** sweep-v2.2's remaining difference was not a fill difference: both
tools leave the GND of the two keyboard halves apart (only the cable joins them) and name different ends
(KiCad a right-half pad and the B.Cu zone, cadlab a left-half hole and a right-half pad). The comparison now
also pairs a KiCad `unconnected_items` with a cadlab `drc.unrouted` when they join the same two islands of
cadlab's connectivity (a zone stands for each of its fill islands). No pour-connectivity known entry is left.
**DRC tolerance kept at 2 µm.** Measured with the router benchmark (`RAYON_NUM_THREADS=1`): with the DRC at
KiCad's 0.5 µm and the router's 1 µm slack (`router::index::TOL`), qfp-qfn-4l and fine-4l leave 3 connections
unrouted (ripped up by the final DRC); tightening the router's slack to 0.45 µm or 0.25 µm keeps every board
complete but moves its grid legality and sampling margins, so routes change beyond the benchmark's limits
(fine-4l +2.8 % length, bga144-4l +10 vias at 0.45 µm). The slack is spread through the router's grid,
fanout and pair code that other work is changing, so it is left as it is: deficits between 0.5 and 2 µm stay
KiCad's alone, far below any fab's resolution.
*Why:* each residual was traced to a measured cause; the copper rings and the row spokes are design copper
the fab makes, and the island pairing states what an open connection is (two islands), not which ends a tool
prints.

### D61. Router applies scoped custom rules, local clearances and hole clearance; flow-through pair parts in series (2026-10-15)
The router now keeps the clearance the DRC resolves for each pair of items (`board::clearance`: last custom rule
holding either item, pair scopes included, else the larger local clearance, else the larger class clearance, never
under `min_clearance`), where it used the net classes only. Existing copper gets its rules exactly. New copper's
rules depend on where it is, so every rule scope that can hold a track or via becomes a condition (kinds, layer,
courtyards and areas as polygons) and new copper has a state per condition — surely held, surely not, or maybe
(the band of its own reach around a boundary, 1 µm margins; 5 µm for "entirely inside", whose DRC test uses the
copper's polygon) — as two bitsets (`router::scoped`); the clearance used is the largest any combination of the
maybes resolves to. The grid reads per-layer state rasters whose samples cover every point of a move through them;
exact checks split segments into pieces of uniform state (bisection to 20 nm). Because the DRC decides a whole
item (a track merely reaching into a courtyard is held everywhere, one partly outside an area nowhere), routed
tracks are split where their state changes (main router, fanout, pairs, push-and-shove), so each output track is
held uniformly and decided as assumed. Plated holes of pads and existing vias keep `hole_clearance` from other
nets' copper (D56 added the check, not the router side: glasgow lost 26–28 connections per run to it and kept up
to 22 errors from fanout vias), and a new via keeps the larger of pad + clearance and drill + hole clearance.
Fanout heuristics use the clearance in effect near the pad, and neck-down widths never go under a track width
rule that may hold them. With no custom clearance rule, local clearance or hole clearance every value is the
classes' as before: the generated-board baseline is identical. Tried and dropped: a grid pitch from the smallest
rule clearance (no gain on a QFN test board, twice the time) and an escape spacing using the per-point states
(more escapes too close together: tinytapeout-demo fell 2 points). Corpus: tinytapeout-demo 83.2 % → 93.7–95.3 %,
glasgow-revC3 93.0 → 94.9 %, buspirate5-rev10 99.6 → 100 %, glasgow DRC errors 1 and 22 → 0; corne-cherry
98.6 → 97.7 % (it is budget-bound and its local clearances change the negotiation; ignoring them reproduces the
old result exactly, but that result relied on clearances the DRC does not apply).
**Pairs.** The "polarity" failure D55 worked around by holding pair ends still in improvement had two causes: the
pair's spanning tree hung a flow-through ESD array off the receptacle–MCU link when those two were closer to each
other than to its far side (a stub through the array), or joined a side that makes the nets cross; and an end part
moved by improvement could bring its breakout onto a passive. A flow-through part (not interleaved contacts) now
sits in series: its two sides first join the ends of two other parts (the shortest such pair), and when a
connection fails on polarity the outside ends swap sides and the pair is routed again (kept when fewer fail).
Placement checks a moving pair end's breakout areas against the passives, as passives are checked against them.
The improvement hold is removed; spread's group moves still keep pair-end groups in place (removing that too
leaves session 3's ESD array mirrored past the MCU, and an unrelated net then fails to route: an arrangement
choice, not the router's fragility). A focused test moves the receptacle through 21 positions (12 failed before).
*Why:* boards carry their design rules as custom rules scoped to parts and areas (tinytapeout's QFN-80 is routable
only under them), and a router that is more or less permissive than the DRC either fails connections the design
allows or has its work ripped up afterwards; a conservative band at boundaries plus splitting the output keeps the
two in agreement without a geometric model of every rule term.

### D62. Session 4 follow-up: KiCad DRC exclusions as waivers, keep-together hints, place.near over routing, route.nets shoving leftovers, KiCad zone fills, supplier part numbers from fields (2026-10-15)
Session 4's open items (docs/TESTING.md), except the schematic netlist reader (D13 stands).
**DRC exclusions.** D58 left them out because KiCad keys them on item UUIDs and marker positions; the board
import already keeps a KiCad UUID → cadlab object table, so an exclusion (`type|x|y|uuid|uuid`, or a
`[entry, comment]` pair) becomes a waiver when its type maps to cadlab codes, every item was imported (edges
dropped) and cadlab's DRC of the imported board has a finding of such a code naming all the items. The
waiver's subjects are the items, not the finding's full subject list, so it keeps matching what KiCad meant.
Matching against a real finding (one DRC run at import, only when there are exclusions) avoids waivers that
would sit unused; the rest is reported per type and cause (`import.drc_exclusion`). Positions are ignored:
items identify a finding, and cadlab's markers are not KiCad's.
**Keep together.** `place.hint --near X` (stored in the hint, follows rename, dropped with X's removal; loops
refused). A passive's target becomes X's pad of a shared net (signal, then power, then ground), with the
decoupling capacitor's pull (4) and a tether of 4 (above decoupling's 2): the hint says the author's intent,
so it outweighs any net. An anchor is pulled to that pad and waits for X when X is an anchor being placed.
An explicit hint rather than a heavier weight for shared signal nets: the LED itself has no anchor to sit at
(its nets are PWR_LED and GND), so the pair drifts with whatever pulls the resistor, and only the author
knows the two belong together. On the focused test's board (LED sent to an edge) the resistor went from 9.4 mm
to 1.8 mm from the LED.
**place.near on a routed board.** D58 made placement treat tracks and vias as obstacles for the whole
courtyard, which leaves no room on a routed board. Copper under a part's body is legal (only pads must keep
clearance), so place.near first keeps the courtyard clear (unchanged results where it fits), then searches
again with the routing removed from the free area and each pad checked against tracks and vias of other nets
(board or net class clearance, `distance_less_than`); the nets under the body are reported
(`place.over_routing`). `place.auto` keeps the stricter rule: a new part placed by the whole-board search can
always find a clear spot or say there is none, and keeping routing out from under new parts leaves the
router room for their escapes.
**route.nets leftovers.** The router gives `Scope::Nets` leftovers one push-and-shove attempt but not the
rip-up fallback of `Scope::Connection`; adding it inside the router would touch the negotiation code another
workstream is changing, so `route.nets` (option `shove`, default on) calls the router once per leftover
connection with `Scope::Connection` and `shove`, applying each success, within the remaining budget. `route.all`
is unchanged (benchmark numbers), but its failures, like `route.nets` with `shove: false`, carry a first hint
naming `route.connection A B`, the command that can move other nets.
**KiCad export fills.** Fills are derived data in cadlab, but KiCad treats the file's fills as the board's
copper until refilled, so an export without them showed every zone-connected pad unconnected. The export
writes cadlab's fill per zone layer as `filled_polygon`s (polyclip's `fracture_set`: one hole-free outline per
island, as the format stores fills) and `fill yes`. Import still recomputes fills (`import.zone_fills`).
**Supplier part numbers.** Netlist fields such as `LCSC` were already kept as component properties (the
schematic's data). They stay there and are interpreted at export only, so the design stays provider-agnostic
(D12): `supplier::SKU_FIELDS` maps field names to providers; a BOM line carries the numbers all its components
agree on; a fab whose catalog provider has one uses it for the BOM's part number column when no catalog lookup
found one (recorded as `sku_from_fields` in fab-lock.json) and offers it first as a substitute (basis `field`,
found through `Provider::lookup_sku`, a default keyword search kept to exact SKU matches). KiCad symbol library
imports still drop such fields (`import.symbol_supplier_field`): a library part is not one design's choice.
**Output.** `fab.export` prints the directory once and file names wrapped at ~100 characters, with paths in
its output relative to the project; `circuit.list` shows the footprint a placement uses (and the part's when
different); `bom.list` words generic, placeholder and other MPN-less lines apart.
*Why:* each item was a step where the session agent fought the tool or worked around it; the fixes keep the
design data unchanged in meaning (waivers are recorded decisions, hints are intent, supplier numbers remain the
schematic's fields) and keep router internals untouched.

### D63. Full-wave losses: surface impedance on conductor loops, Djordjevic–Sarkar dielectrics as fitted Debye terms (stackup `ref_freq`), solder mask layers (2026-10-15)
Closes the limits D59 listed for `sim.sparams`. **Dielectrics:** `Dielectric` gains an optional `ref_freq`
(exact decimal in Hz, `board.dielectric --ref-freq 1GHz`; `"0"` clears): εr and tan δ are given there and the
dielectric follows the Djordjevic–Sarkar wideband Debye model (IEEE Trans. EMC 2001; corners fixed at 10 kHz
and 1 THz, ε∞ and Δε in closed form from the two values; `electrical::dispersion`). The 2D models use it to
first order: the field solver now keeps the partial capacitance of each group of regions sharing (tan δ,
`ref_freq`) (`LineMatrices::groups`), so C(f) = C + Σ P (εr(f)/εr − 1) and G(f)/ω = Σ tan δ(f) P εr(f)/εr
need no new solve per frequency (within 0.03 % / 0.2 % of a full re-solve); the closed forms take P = C ×
their filling factor. `impedance.calc` and `net.loss` take a `ref_freq` override (impedance itself stays at
the stackup's εr). The FDTD needs
a causal model for every lossy dielectric: Djordjevic–Sarkar at `ref_freq`, or at the pulse's center f₀ when
none is set (the old constant σ was exact only at f₀ and is replaced). Each is fitted over the result's band
(`fdtd::fit`) as ε∞ + σ/(jωε₀) + Σ Δεₚ/(1 + jωτₚ) on relaxation times shared by every material (three per two
decades, a third of the band below to three times above), weights by Lawson–Hanson NNLS (non-negative:
passive, ε∞ ≥ 1); poles update by the auxiliary differential equation (bilinear in J), edges average ε∞, σ and
each Δεₚ of their four cells. The energy criterion counts the polarizations (P²/2ε₀Δε, P = ε₀Δε E − τJ).
**Conductors:** with a conductivity (default 58 MS/m as D52; `perfect_copper` keeps PEC; `roughness`,
`huray_radius`/`huray_ratio` as D52) a surface impedance boundary condition (Maloney & Smith, Beggs et al.,
both IEEE AP-40 1992): Zs = Rs K + j Rs fitted as r₀ + Σ rₚ s/(s + aₚ) (non-negative sections, recursive
bilinear convolution). The tangential E of a surface edge is Zs times the current through the edge's dual loop
(H circulation over its faces outside the conductor) divided by the copper surface that loop encloses, and it
enters those faces' H updates: dissipation Rs I²/w. Inside a sheet each side is its own loop; at a sheet's
edge one loop. Domain walls count as conductor for "inside" and surface extent. **Mask:** `export.openems` and
`sim.sparams` take `mask`, `mask_copper`, `mask_er` (3.8), `mask_loss_tangent` (0.025), the field solver's
options: a dielectric over the laminate and over the outer copper on both sides, open at pads with a mask
opening, surfaces on mesh lines; in the export a `solder_mask` material plus `mask_openings` air primitives by
priority, and dispersive dielectrics written with εr and κ at f₀. **End criterion:** `sim.sparams` defaults to
60 dB (was 40). Validated (tests/fdtd.rs, ignored ones in release): cavity Q with copper plates = h/δ within 1 %
from 1.4 to 14 GHz (and K with Hammerstad–Jensen, Huray), Djordjevic–Sarkar cavity resonance within 0.2 % and
Q = 1/tan δ(f) within 1 % from 2.4 to 15 GHz, microstrip conductor loss against the field solver's within
−5.7 to +3.3 % (3–8 GHz, 4 and 8 cells across), εeff(f) of a dispersive microstrip within 1 %, the full model's
|S21| on a 60 mm line within −9.5 to +5 % of D52's total from 0.5 to 9.5 GHz (−0.8 to +5 % from 3 GHz),
masked microstrip εeff within 0.5 %, bit-identical for any thread count.
*Why:* a surface impedance is the textbook way to put skin-effect loss on FDTD conductors without meshing the
skin depth, and the rational fit keeps it in the time domain and passive. Spreading each edge's current over the
copper it can flow on, rather than taking each face's H as the surface field, is what makes edges and corners
right: in the 2D solution the corners and the 35 µm sidewall of a 1 mm microstrip hold ~40 % of its conductor
loss, which per-face sampling lost (40 % too little, converging as size^(1/3) — no affordable mesh fixes it),
while the loop current is exact by Ampère's law on any mesh. A wideband Debye model is causal (a constant tan δ
is not) and is what laminate vendors' data fits; giving it one reference frequency keeps the stored data two
decimals and a frequency per dielectric, and leaves dielectrics without one exactly as D52 had them in the 2D
models. Fitting shared poles, rather than one pole set per material, makes the interface average exact and the
ADE cost one state per pole per edge. Six poles on a two-decade band hold the model to 0.1 % in εr. The
board-level residual (−9 % at 1.6 GHz to +5 % at 9 GHz) is the open-region line measurement's, not the models'
(the cavities agree to 1 %; a constant-conductivity line, whose loss is flat, measures 0.30–0.39 Np/m there).
The export does not write openEMS's dispersive materials or conducting sheets: their XML attributes are not
documented well enough to write without reading openEMS's code.

### D64. Router escapes planned per package as an assignment, re-planned for boxed pads; negotiation estimates costs near the targets (2026-10-15)
**Escapes.** Fine-pitch escapes were planned greedily pad by pad (shallowest exit first, up to four attempts with
the pads left over first), so in a tight row every other pad found its exits taken by its neighbors' (25 of the 75
off-grid pads of tinytapeout-demo's 0.4 mm QFN-80, 6 of 28 on an RP2040 of corne-cherry). Now every pad of a
package gets its candidate exits (up to 24, shallowest first, each legal against everything placed so far) and one
exit per pad is chosen for the whole package before any is committed: pads in order take their best candidate free
of the chosen ones, then each pad left without one tries its candidates, moving the chosen exits in its way (at most
three) to other candidates of theirs, with chains of up to two further moves (ejection chains, as in local search
for assignment problems), until no pad gains an exit; two candidates clash when their tracks break a clearance or one
ends in the other's halo as the grid samples it. The count never drops and everything follows the data's order, so
it is deterministic. QFN-80 50 → 65 exits, RP2040 22 → 27. **Neck-down for crowded rows:** a single-layer pad of a
fine-pitch row whose class width gets out of the pad but cannot run beside its neighbor's track at the pad pitch
with the clearance outside the part (tinytapeout's 0.22 mm Default class out of the QFN-80, 0.16 mm under the
courtyard rule, 0.2 mm beyond it) is necked like a pad its width cannot leave (0.185 mm there), since otherwise no
assignment gives every pad an exit. **Re-plan:** when a run leaves a pad that had candidates without an exit and
its connection fails, the nets of that pad and of its package's pads within three pitches (nets of at most 8
connections) are ripped up and routed again with the boxed pads served first; kept when fewer of their connections
fail (needs a twentieth of the budget left). Pads without any candidate (USB-C pads walled in by the receptacle's
own pads) are not re-planned. **Stub conflicts:** access stubs are off the grid, so the halos saw them only at grid
samples and two stubs of different nets could pass too close (the DRC then removed a connection; it showed on
corne-cherry once its escapes changed). Grid wires' stubs are now checked exactly against the other nets' off-grid
copper (stubs, gridless wires) when conflicts are counted, and searches that may not share drop stubs that are not
clear of any committed routing. Checking stubs against grid copper too counted conflicts the halos already hold, at
other points, and changed negotiation (the LQFP-48 test lost a connection with the grid search alone).
**Time (corne-cherry exhausted its budget since D61).** Profiling the corpus re-route (stack samples of the running
router) put 92 % of corne-cherry's time in grid A* expansions, not in scoped-rule lookups: late in negotiation the
history and present costs around a connection's targets are a cost every path pays and the distance heuristic does
not see, so single connections flooded up to 350 000 nodes. A search still running after 10 000 expansions, from the
sixth iteration on (present factor 5 or more), now settles the 5 000 nodes nearest its targets backwards (Dijkstra
over the moves the search may make, on its costs without bends) and estimates every node by the larger of the
distance and that cost, capped at the last cost settled, which bounds every node not settled (Dijkstra's order).
Still a lower bound and consistent, so paths stay optimal; entries already queued keep smaller keys and only come out
earlier. Same searches with and without it: 0.51× the time on corne-cherry, 0.32× on cynthion, 0.9× on
glasgow-revD1. Applied from the first iteration or after fewer expansions it was also faster on corne, but changed
the generated boards' negotiation more (stm32-2l 57 → 65 vias); gated as above the generated-board baseline moves
within tolerance only (stm32-2l one via more, bga144-4l 0.3 % shorter; re-blessed). Tried and dropped: a weighted
heuristic in negotiation (slower per iteration on corne: more re-expansions), node data interleaved in one array
(no gain), rerouting only nets whose routing passes near an overused point instead of those whose box holds it
(conflicts sit where everything passes; no smaller sets). Corpus (ROUTER.md): corne-cherry 97.2–97.4 % in 60 s
(budget) → 97.7 % in 47 s, tinytapeout-demo 93.6 → 95.6–96.7 %, glasgow-revD1 93.2–93.4 → 93.7 %, lumenpnp-mobo
99.1 → 99.3 %, cynthion converging inside the budget, buspirate5-rev10 26 → 9 s; zero DRC errors throughout.
*Why:* exits are a shared resource of a package, so planning them one pad at a time decides the conflicts by pad
order; an assignment with local repair serves the pads the geometry allows, and the re-plan lets the routing result
say which pads mattered. The router's time went to searches whose costs the heuristic ignored, so the estimate, not
the clearance bookkeeping, was the thing to fix.

### D65. Full-wave solver: board parts as lumped series R–L–C elements, from their part values (2026-10-22)
`sim.sparams` models the two-pin resistors, capacitors and inductors of the region (`parts`, default true):
each is a series R–L–C (`resistance`, `capacitance` or `inductance`, plus the new known parameters `esr` and
`esl`; a component property overrides the part's parameter; 0 Ω is a short of conductor edges) placed on its
pads' copper layer along x or y, in the mesh edges between the two pads' copper, over the nodes where the
pads face each other (both surfaces of solid copper). Columns are in parallel, the edges of a column in
series, sharing R, L and 1/C by length. The engine's new `LumpedElement` keeps the element current and
charge per edge and advances them with E by the trapezoidal rule (E l = R I + L dI/dt + q/C at the half step,
(I⁺ + I)/2 in Ampère's law): a 2 × 2 elimination per edge after the curl update, serial (few edges), so the
results stay bit-identical for any thread count; the stored energy counts in the end criterion; element
edges take the Debye terms' permittivity at f₀ instead of their own polarization state. Parts that cannot
be placed (no value, not two pads, a pad outside the region, pads on two layers or not facing along an axis,
no cell between the pads) are reported (`sim.part_not_modeled`) and their pads stay open; DNP parts are open
with a note; the output lists the parts modeled. Validated (tests/fdtd_parts.rs) against circuit theory
between 50 Ω ports: series R within 0.001 in |S₂₁|; series C, L, R–L–C and shunt C, R–L–C (a notch to 0.167)
within 0.016; on a board microstrip through `sim.sparams`, a 0603 part as R, C and C with ESL/ESR within
0.015 of the ideal element from 0.5 to 6 GHz (the pads' own gap and steps). The openEMS export does not write
the parts yet.
*Why:* AC-coupling capacitors and series terminations sit on the nets agents simulate, and their pads alone
are an open circuit. The trapezoidal (semi-implicit) update is the textbook way to add reactive lumped
elements without lowering the timestep (an explicit inductor current is only conditionally stable for small
L), and passive for any non-negative values; a series R–L–C covers the parts with a datasheet ESR/ESL, while
an ideal element at the BOM value is what a part is when nothing else is known. Placing the element in the
gap edges found on the voxelized copper, rather than adding mesh lines at the pads, keeps the openEMS mesh
(shared with the export) unchanged. Curved copper still staircases: a conformal method was not cheap enough
for this step.

### D66. Agent session 5: mixed-signal board with blocks: first instance at capture, net ties in ERC/SPICE/BOM, placement regions, split-crossing DRC (2026-10-22)
Session 5 (docs/TESTING.md) designed a 2-channel 24-bit data logger on four layers through the MCP server only
(STM32F411, ADS1220 with REF5025, an op-amp front end as a block instantiated twice, AGND/GND joined by a net-tie
star point under split pours, two LDOs, USB-C, microSD). What it changed:
**Blocks.** `block.create` takes `instance`: the captured components become the block's first instance (they keep
their designators, belong to the instance, internal nets are renamed `<instance>/<net>` with their copper) instead
of staying outside it, which took a remove and two instantiations; without it an info `block.not_instance` says
so. Components already in an instance are refused (`block.component_in_instance`). `circuit.list` shows and
filters by instance, `circuit.summary` lists instances with their members. `place.hint` and `export.spice
--components` accept an instance name for its components.
**Net ties.** A component whose footprint's pads all sit in net-tie groups is a net tie (`board::is_net_tie`): ERC
powers a net reached through one, like through a ferrite bead (AGND fed from GND at the star point; the
`erc.power_not_driven` hint for a ground net now names a net tie before `--driven`); SPICE writes it as 1 mΩ per
tied net, so a split ground keeps its DC path to node 0 (it was a commented-out placeholder that left AGND
floating); BOM lines, placement files, X3 component layers and STEP bodies leave it out (copper, nothing to buy or
place), and the KiCad export marks it `exclude_from_pos_files exclude_from_bom`. Net ties stay data of the
footprint (D40), with no flag of their own.
**Placement.** `place.hint --region {from, to}` keeps parts' courtyards inside a rectangle (the analog section);
enforced in the placer's validity test, so every stage (anchors, passives, improvement, spreading, swaps) respects
it. An explicit edge hint now wins over the rooms of groups placed earlier: the part takes the valid poses
closest to its edge (best by score among them, with the largest room still clear), where before a left-edge
connector went to the top because the analog groups' rooms covered the left edge.
**DRC.** `drc.split_crossing` (warning): a signal net whose tracks on a layer run over the fills of two different
ground nets on an adjacent copper layer, one finding per net, layer and plane layer, located where it changes
ground. Only ground pours are reference planes and only signals are checked (supply rails reach both sides and are
decoupled on each). Intended crossings are waived with a reason, as the session did for the ADC's SPI lines.
**ERC output** takes `severity`, `codes`, `limit`, `offset` and reports `by_code`, `listed`, `not_listed` like
`drc.run` (an ERC before wiring was 37 KB of open pins). **Moves of routed parts** (`place.set/move/rotate/flip`)
warn `place.moved_routed` with the nets whose tracks ended on the old pads and the rip/route commands; `rip: true`
removes that routing (`place.ripped`). **SPICE** exports a subset (`components`, designators or instances): one
channel simulated on its own. **Schematic** ground symbols print their net's name unless it is `GND`, so AGND and
GND read apart.
Deferred: `part.set` cannot change one pin's kind (the session re-sent the whole pin list with `part.create
--replace`); the placer still put the USB ESD array's flow-through pins so that the pair crossed once (fixed by the
spread run that followed, but not by construction); `describe part.create` is 26 KB (the `package_spec` schema).
*Why:* hierarchy, split grounds and partitioned placement are what make a board mixed-signal, and each of them
forced a workaround (remove and re-instantiate, mark AGND driven, place analog parts by hand, waive nothing because
nothing checked the split) or produced wrong outputs (a floating AGND in SPICE, a net tie in the pick-and-place
file); the fixes keep the design data's meaning (a net tie is still a footprint property, regions are hints like
edges).

### D67. Spread placement: crowding over the density, jumps to where connections pull, a ratsnest budget per move, placement stats (2026-10-22)
Spread placement still left parts of roomy boards empty, and measuring it (new `board::place::placement_stats`,
reported by `place.auto` as `board_use`: about 16 windows over the free area, those nearly empty, the crowding,
the unevenness, the largest empty square, the extent; `tests/placement_quality.rs`) showed why. **All or
nothing:** D55's relaxation pulled every window towards its even share; on a sparse board (the ATtiny board on
80 × 60 mm, four op-amp channels on 100 × 80 mm) its ratsnest overshot the 25 % budget at every stretch, and the
fallback undid every group move. Each move is now checked against the budget (a move that shortens the ratsnest
is always allowed), so the moves that fit stay. **Density, one-sided for groups:** the window term is now the
excess of a window's courtyard area over its share divided by the density (1.67× at 60 %: the parts must
spread over at least that share of the board), so crowded groups move apart and nothing pulls a group away
from its connections into an empty window (with the jumps below, the two-sided term sent the ATtiny's
regulator to an empty corner, 30 mm from its connector). Groups bound to an edge (connectors, edge hints) keep the shortfall term:
where a connector sits along its edge is free, and filling an empty stretch of edge brings its group's
connections with it (session 3's board still uses every third of its width).
**Moves:** besides the 1–8 mm shifts and mirror images, a group may jump to any window's center or to where
its connections pull it (mean of the outside pins of its signal nets, rails of more than eight pins and
ground left out, and a 4 mm ring around it); edge-bound groups only along their edge, and a lone connector may
turn, seated back at its distance from the edge, paying for lying across it (also in the improvement, which
no longer turns a connector across its edge). **Pivot:** the central IC stays only when it dominates (half as
many pins again as any other IC); among peers every group moves (the four channels had clumped around the
first op-amp). Results (D55 → D67, spread): STM32 board on 100 × 75 mm ratsnest 1558 → 1387 mm, empty windows
6 → 5, vias 93 → 76 with routing 108 → 107 of 109 (failures at the MCU's pins, a group not moved); channels
on 100 × 80 mm 716 → 529 mm (each op-amp at its connector, less than the packed 625 mm), on 60 × 45 mm
506 → 394 mm, empty windows 4 → 1; STM32 on 60 × 45 mm routing 106 → 108 of 109; the ATtiny boards ±5 %
ratsnest; the 528-part synthetic board (`big_board_spread_timing`) places in 47 s instead of 59 s (the
budget-limited relaxation stops earlier) with a 0.7 % shorter ratsnest. `compact` (the router benchmarks'
placement) is unchanged.
Tried and dropped (grid over the variants on six boards and session 3): the crowding term in the per-part
improvement (spread passives and anchors a little, +10–20 % ratsnest on small boards), a floor letting any
window hold a quarter of its area (on sparse boards the shortfall term then sent connectors away from their
groups: channels 529 → 706 mm), the shortfall scaled by the board's fill, connectors' lying across weighted per
pad or ranked before cost (both turned headers at a large ratsnest cost: STM32 100 × 75 mm 1387 → 1540–1620 mm),
and a stronger across penalty in the constructive placement. The stats keep that floor: crowding counts only
beyond a quarter of a window.
*Why:* an analytical placer's model — wirelength subject to a density limit — is the one whose optimum is a
sensible board: groups apart where they would crowd, next to what they connect to otherwise; the density
option already said how much of the board to use, so it sets the limit. Judging moves one by one against the
ratsnest budget keeps the budget's meaning (how much using the board may cost) without discarding everything
when one stretch overshoots. Jumps to where connections pull fix what small shifts could not reach: a group
with its connector across the board. A number that says how the board is used lets agents and tests judge a
placement without a picture. Remaining: long headers can still end up across their edge (three on the
STM32 100 × 75 mm board, as before), large pinned groups (the MCU with its satellites and passives) stay dense
since only whole groups move, and on sparse boards the middle stays empty when every group sits by its
connector.

### D68. openEMS export at parity with `sim.sparams` (Debye materials, conducting sheets), an openEMS oracle in CI, the CPML's frequency shift at f₀ (2026-10-22)
**Export.** `export.openems` now writes the model `sim.sparams` simulates. A lossy dielectric (and the solder mask)
is a CSXCAD `DebyeMaterial` holding exactly the solver's fit (`fdtd::fit::debye_fit` over the Touchstone band on
the shared relaxation times: ε∞ as `Epsilon`, the conductivity as `Kappa`, each non-zero Δε and its τ as
`EpsilonDelta_n` / `EpsilonRelaxTime_n`, 7 significant digits); lossless ones stay plain `Material`s. Copper takes
`sim.sparams`'s `conductivity` (default 58 MS/m), `perfect_copper`, `roughness`, `huray_radius`/`huray_ratio`
(shared validation, `export::copper_options`): each net's copper per layer becomes a `ConductingSheet`
(`Conductivity`, `Thickness` = the layer's copper) at priority 11, on both surfaces of solid copper or as the sheet
itself; roughness becomes σ/K(f₀)² (openEMS's sheet has none). Vias stay perfect `Metal`. A perfect conductor kept
between the two sheets of solid copper cost 40 % of the conductor loss in openEMS (0.011 against `sim.sparams`'s
0.018 dB at 1 GHz on a 20 mm line with lumped ports); without it 0.017 against 0.018, 0.031 against 0.030 at 3 GHz.
Three bugs the first real openEMS runs found: the geometry file was wrapped in `<openEMS>`, which
`ContinuousStructure.ReadFromXML` rejects ("No ContinuousStructure found"; it is now a bare
`<ContinuousStructure>`, as `Write2XML` writes and AppCSXCAD opens); the driver let the simulation object (which
owns the structure) die before `CalcPort` read its grid, and `Run` needs its directory to exist; and `AddMSLPort`
without `Feed_R` has no termination (openEMS's documentation: the line must then end in an absorbing boundary;
ours end inside the region), so the unexcited port reflected everything and the run never decayed — every
microstrip port now gets `Feed_R` = z0. openEMS's microstrip port also reads nonsense on a strip with thickness
(εeff 3.6–6.8 for a 3.0 line), so `msl` ports on solid copper are warned (`openems.msl_solid_copper`).
**Oracle.** openEMS has no Ubuntu package or Linux wheel; a CI job (`openems`, informative like the KiCad oracle)
builds openEMS-Project at a pinned commit (`update_openEMS.sh --python --disable-GUI`, about 4 minutes after 6 of
apt, cached per commit) and runs `tests/openems.rs` with it. `lossy_microstrip_oracle` runs a 24 mm lossy
microstrip through both solvers twice. Sheet copper between microstrip ports: openEMS Z₀ 50.3–52.8 Ω against
`sim.sparams`'s 50.8–51.0 Ω and the 2D solver's 50.8 Ω; conductor loss 0.015 against 0.014 dB at 1 GHz. Solid
copper between lumped ports: conductor loss 0.017 against 0.018 dB at 1 GHz, 0.031 against 0.030 at 3 GHz. The
total |S21| is 0.02–0.05 dB higher in openEMS (its εeff 3.24–3.30 against the 2D solver's 3.07 puts more field in
the laminate); the test allows 15 % + 0.025 dB on |S21| and 5 % on Z₀, and writes the tables to the job summary.
The golden via transition's openEMS run is opt-in (`CADLAB_OPENEMS_VIA=1`): openEMS diverges on it from about
15 000 steps, with the GND planes present, also with perfect copper and constant-κ dielectrics (so it was there with
D54's export, which no openEMS had run), while `sim.sparams` passes it; and `boundary: mur` diverges on any region
with a dielectric. Not resolved.
**CPML.** The −9.5 % board-level loss at 1–2 GHz of D63 was not the loss models: the same 60 mm line lossless read
up to −0.06 Np/m (|S21|² + |S11|² = 1.007, a gain) around 1.8 GHz, independent of the line's length per meter,
of the end criterion and of the dielectric or conductor model, and gone with the side margin at 12 mm. The CPML's
complex frequency shift was 2π ε₀ f₀/10 (0.028 S/m at 5 GHz), a tenth of the 0.05–0.3 S/m range Taflove and Hagness
give; the shift is what makes the layer absorb evanescent fields, and with too small a value the line's near field
in the side PML 4 h from the strip exchanged energy with it. The shift is now 2π ε₀ f₀ at the layer's inner face,
falling as (1 − ρ)² into it (falling linearly, the deep lossy part kept too much shift and absorbed a line running
into the end PML too little below f₀: Z₀ −2.3 % at 1 GHz in `microstrip_fine_with_loss`; a κ of 5 did not help the
near field at all). The lossless line is passive within 0.1 % in power (|S21|² + |S11|² ≤ 1.001) with the same
5 mm margin and the full model's |S21| is within −2.3 to +5.0 % of D52 from 0.5 to 9.5 GHz
(was −9.5 to +4.9 %). Left: with a linear α the conductor-only reading of that line was still 8–12 % low from 1 to 3
GHz on this coarse mesh (3–4 cells across the strip); and the engine-level dispersive microstrip, which runs into the
end PMLs and measures with probes on the line, now reads α 10–17 % high at 1.5–2 GHz (+3 % before; its check stays
3–6 GHz, within −4 to +4 %): below f₀ a stronger shift absorbs a line entering the layer less well, so lines
measured against a PML at low frequencies want `f_min` (a lower f₀) or a thicker layer.
*Why:* the Debye form is exactly what `sim.sparams` integrates, so writing its fitted terms (rather than εr and κ
at f₀) makes the two solvers simulate one model and the oracle compare solvers, not models. The attribute names
come from CSXCAD's documented material parameters (`SetMaterialProperty`'s `EpsilonDelta_n`, `EpsilonRelaxTime_n`;
`AddConductingSheet(conductivity, thickness)`), with the element names CSXCAD's own `Write2XML` gives those
properties, checked by reading files back through openEMS's Python interface (black-box, no code read). The
conducting sheet is openEMS's documented model for lossy traces; putting it on both faces of solid copper keeps
the solid geometry for lumped ports, while microstrip ports need sheets anyway. Building openEMS in CI is ten
minutes once per pinned commit, which is reasonable for an informative job; installing it on developer machines is
not required (the oracle skips). The CPML fix is a parameter of the textbook method, not a work-around: margins
large enough to keep the PML out of every line's near field would cost cells on every run.

### D69. Router: USB-C crossovers leave the outer pads an exit, escapes may turn earlier, the boxed-pad re-plan gets its time, large nets keep wires away from the overuse (2026-10-22)
**USB-C pads without exits (corne-cherry).** The receptacle's CC1 pad and the D+ pad beyond the crossover had no
candidate exit at all. Not the pad geometry or a keep-out: the crossover via (D57) sat at the shallowest depth from
which its two row neighbors could pass it, so their exits turned 45° towards the next pads right past the pad ends,
0.12 mm into their lines; those pads could neither go straight on (0.38 mm to the turned track, 0.45 mm needed)
nor turn earlier (a no-connect pad's corner beside them), and an escape candidate only turned at 45° straight onto
its end cell. Now the crossover via is the shallowest that also leaves each wanted pad beyond a neighbor a way out
(straight, then turning away by as much as the neighbor moves, before it does), the neighbors' exits turn as late
as the via allows, and a pad with no candidate tries exits turning earlier, then running straight on to the cell.
The via moved 0.1 mm deeper; both receptacles route (corne-cherry 97.7 % → 98.8 %, 44 → 31 s).
**Re-plan time (tinytapeout-demo).** The re-plan of boxed pads (D64) needs a twentieth of the budget, but the
gridless rip-up and retry ran to the end of the budget. When connections of pads the escape planning boxed in still
fail after rip-up and retry, the gridless phases and via minimization now stop early by twice the run's setup
time (fanout, board, grid, pad access: most of what the re-plan of a few nets costs; 10 s there), when that is at
most a quarter of the budget. The re-plan now runs and no QFN-80 pad is left boxed in (96.7 % → 97.0 %). A fixed
fifth of the budget was tried first: with a 30 s budget on slower machines it cut the gridless phases without
leaving the re-plan enough time.
**Time per pass (glasgow).** Profiling (stack samples, and per-batch timings) showed the negotiation of both
glasgow boards running mostly on one thread in batches of one or two nets, with GND alone 60 % of revC3's
negotiation (184 connections, 1.3 s per iteration) and 30 % of revD1's: a net is rerouted whole whenever its box
holds an overused point, and GND's box holds all of them. A net of 64 connections or more that is rerouted only for
that reason (no overuse of its own) now keeps its wires whose box, grown by the clearance reach and ten grid
pitches, holds no overused point, and routes the rest from them (partial rip-up, as connection-based PathFinder
variants do); kept wires must still reach the net's pads through each other, else they are ripped too. Iterations:
revC3 1.75 → 0.87 s (40 iterations inside the budget instead of 24, 94.9–95.1 % → 95.4 %), revD1 4.5 → 3.9 s
(93.6–93.7 % → 93.7 %), lumenpnp-mobo 56 → 41 s at the same 99.3 %; over seeds 1–3 (60 s) revC3 failed 106 → 102
connections in total, revD1 311 → 301. Tried and dropped: partial rip-up for nets in
conflict too (2× faster on revC3, but 4 more failures there and 1–3 more at convergence on mobo, cynthion and
revC3), without the ten-pitch margin (the wires beside the congestion stayed put), and applying it to nets of 24
connections (stm32-2l gained 10 vias, fine-4l 4 % length). The generated-board baseline is unchanged (no net
reaches 64 connections). Legalization and retries still route whole nets, and only negotiation honors the kept
wires (a mask left by an iteration cut short once let a retry keep wires through another net's new path: shorts
the verification removed). The remaining serial time is in many single-connection BGA nets of ~100 ms each and in
the unbudgeted verification DRC of post-processing (7 s on revD1).
*Why:* the USB-C failure was a planning artifact (the crossover chose its depth for the neighbors alone); the
re-plan existed but never got time; and the budget went to rerouting a net whose wires were far from any conflict.
Each change is checked against the generated-board baseline (identical) and the corpus (ROUTER.md; CI's 30 s runs
vary by more than the change, so the local 60 s runs and a four-seed check decide).

### D70. Agent session 6: the electrical tools on a 2.4 GHz node: `sim.sparams --plan`, part values per run, results at the frequency of interest, mesh slivers, what a port and an ideal part mean (2026-11-01)
Session 6 (docs/TESTING.md) designed a 2.4 GHz sensor node through the MCP server (nRF52832 QFN48 with Nordic's
reference network, TPS62205 buck, BME280, U.FL on a 50 Ω microstrip, JLC04161H-7628) and used the electrical tools
end to end, which no session had. What it changed:
**Mesh.** `mesh::merge` kept lines closer than `min_cell`: a cluster's kept line (its middle one) could land next to
the next cluster's. A 5 µm sliver in a 58 µm minimum set the timestep of the whole run, a factor of three or four.
A soft line that close to the line before it now goes. Golden openEMS files are unchanged.
**`sim.sparams --plan`.** The command is a query and an MCP call blocks until it ends; a first attempt ran past
280 s with nothing to look at, and the only preview was `timesteps: 5`, which read "energy decayed to -inf dB". `plan`
extracts, voxelizes and checks, reports mesh, ports, parts and warnings with a cost `estimate`, and writes nothing.
The estimate keeps out of the deterministic output anything measured on this machine: it is timesteps and cell
updates, plus minutes at a fixed nominal rate. A run stopped inside the pulse says so (`sim.stopped_in_pulse`).
**Reading a result.** `frequencies` and `band` on `sim.sparams` and `band` on `touchstone.summary`: the extremes of a
1–6 GHz run say nothing about 2.4 GHz, and asking for a second command for the value at the frequency of interest was
a needless step. Reflections carry impedance, VSWR, return loss and mismatch loss; transmissions
`available_loss_db`, since |S21| between 50 Ω terminations mixes mismatch with dissipation and a designer cannot
separate them by eye. `checks` (passivity, reciprocity) with warnings say how far to trust a run. Numbers print as
GHz and ps, not `1000000000 Hz` and `0.00000000000001667 s`. The Touchstone header names region, ports and part values.
**Trying values.** `part_values` overrides part values for one run and leaves the design alone: tuning a network
otherwise meant a new part and `bom.replace` per value, or `part.set` on a part other designators share. Not a
component property edit, because a trial should not become design data.
**Saying what the result is not.** Notes rather than documentation, since an agent reads them at the moment it would
misread the number: `sim.parts_ideal` (nominal values only), `sim.port_replaces_part` (a port on a chip pin is a
termination, not the chip). No cadlab output may read as RF performance: the board network between 50 Ω ports is not
the radio's match, and the nRF52832 specification publishes no load impedance to compare with. Parts on none of the
kept nets are one note instead of a warning each: six false warnings hid the two useful ones.
**Export.** `export.openems` warns `openems.parts_not_modeled`: the export silently left a matching network's pads
open, a result that would disagree with `sim.sparams` for no visible reason. Writing lumped elements into the driver
is left to the openEMS work. `openems.port_in_pml` says how much margin is missing and how thick the layer is.
**Smaller:** `net.loss` takes the mask and etch options of `impedance.calc`, so a line solved with a mask is measured
with it; `process.impedance` in `fab-lock.json` lists controlled lines for the order (`fab.impedance_without_stackup`
without a published stackup); capacitors print in pF below 1 pF (`0.8pF`, not `800fF`, also in part IDs and BOMs);
the assumed loss tangent prints `0.02`, not `20m`; a coplanar solve says so in its summary; `impedance.solve --netclass`
says how to assign nets.
Deferred: keep-outs have no net exception (the feed needs two rectangles, one each side; done in D76); no lumped elements in the
openEMS export; passives have no self-resonance model; `describe` ignores an unknown argument and lists everything
(`name` for `command`). Thread count needs no new option: `RAYON_NUM_THREADS` caps the solver, and an idle server
uses no CPU (a server shows one idle thread per core after its first parallel call).
*Why:* the electrical side had been tested as solvers against references, not as a sequence a designer follows; every
fix above is a place where the number was right and the designer still could not get it, read it, or know what it
meant.

### D71. Items left by session 5: single pin changes, `describe` views, regions passed from ICs, flow-through parts by their connections, connectors along edges at no ratsnest cost, honest board use (2026-11-01)
**Pins.** `part.set --pins {"TRIM": {"kind": "passive"}}` changes single pins (name, kind, group, side), keyed by
number or by a name only one pin has (`part.pin_unknown`, `part.pin_ambiguous`, `part.pin_patch_empty`,
`part.pin_patched_twice`). Nets go by number, so they keep their pins. A new type leaves the drawing alone; a new
name, group or side lays the symbol out again with its sides kept. Imported symbols keep their drawing, and their
pins' sides cannot change (`part.pin_side_imported`).
**`describe`.** Registered schemas stay self-contained (CLI generation and argument checks read them);
`describe` returns a view (`command::schema_view`). First, lossless: the length type (1019 uses in the command set,
87 bytes of pattern each) and repeated subschemas move to `$defs`, which a test checks by expanding every schema
of the command set back. Then, for a schema still over 6 KB, arguments over 2.5 KB are folded to their description,
fields and variants (`x-folded`, listed in `folded`); `path` (`package_spec.qfn`, `pads.shape`, `output.stats`)
returns the part asked for, an unknown step says what fits (`describe.unknown_path`), and `full` the schema as
registered. Bytes over MCP: `part.create` 26 569 → 10 696 (one package family: 1.5 KB), `footprint.set`
25 494 → 8 504, `footprint.generate` 21 679 → 5 781, `sim.sparams` 14 266 → 13 359, `impedance.calc` 11 187 → 10 512,
all 169 describes 427 → 364 KB, the command listing 33 → 21 KB (summaries once, in the text). Flat lists of arguments
(`sim.sparams`, `board.rules`) barely shrink: their descriptions are what an agent needs, so nothing there is folded.
`tools/list`, what every agent pays up front: 50 900 → 41 923 bytes (shorter wording of the properties repeated in
32 tools, the `describe` reminder moved to the instructions, `: string` left out of signatures, labels cut at 48
characters with `...`).
**Regions.** Passives (through their chain) and satellite ICs take the region of the anchor they go with unless they
have their own region or an edge hint. An inherited region gives way when full (`place.region_full`, info, naming
the parts) instead of leaving a part unplaced; an explicit one still does not. Session 5's VCAP1 capacitor stays
on the digital side and its `drc.split_crossing` is gone.
**Flow-through parts.** The first attempt, straight lines between the pads crossing or not, was wrong: a rotation
does not change which way a pair's nets lie on leaving a package side (`board::place::pairs`). Each pad pair has a
hand, the side of its way out the positive pad is on; two exits join uncrossed when their hands differ. So a
rotation cannot create or remove a crossing; it decides whether the side that suits a neighbour faces it or the pair
has to turn around the package, which fails where parts are close (the router then reports polarity) while the
ratsnest barely sees it. With the anchors placed, a flow-through part takes the rotation with the fewest crossings, then room
to break out, then the least turning (1 − cos of the angle off the way out, divided by the distance: a near neighbour
leaves no room to turn), then length and pull. Its seat then stays, since improvement would pull it by its rails
into the pair's room (session 5's VBUS, boxed between the pair's pads, lost its exit over 0.5 mm). When spreading,
pair ends also keep their breakouts clear of each other, and improvement never moves one into more crossings. A
crossing no placement can cure warns (`place.pair_crossing`; `board::place::pair_crossings`). Compact placement is
unchanged. `flow_through_part_keeps_its_pair_uncrossed` routes a USB front on seven outlines: 3 of 7 failed
with the orientation and breakout rule switched off, none with them.
**Connectors along edges.** Why D67's attempts cost 10–17 %: a header across its edge reaches into the board, its
pads on average half its length nearer what they connect to, and those attempts turned it where it stood. Now,
after the group moves, each connector across its edge may take any place along any edge with its group turned
rigidly around it, the one shortening the ratsnest most first, so that what one gains pays for the next; the result
is kept only if the improved ratsnest ends no longer than without laying any (slack on the way of 12, 6, 3 and 0 %,
since improvement wins some back; over 200 parts one attempt without slack, from the improved placement). Long
connectors across their edge and ratsnest, before → after: STM32 100 × 75 mm 3 → 1, 1387 → 1370 mm (vias 76 → 70, 107
of 109 routed before and after); STM32 60 × 45 mm 2 → 1, 893 → 863 mm (108 → 107 of 109 routed); channels 60 × 45 mm 4 → 0,
394 → 371 mm; channels 100 × 80 mm 4 → 0, 529 → 509 mm; the ATtiny boards ±0.5 %. **The last header is not free:** the
STM32 board's remaining one costs 9 % more ratsnest (1370 → 1497 mm, `place.auto --along-edges` lays them all), which is
the geometry above, with the MCU at the center and ten pins, so it is left to the user. Tried and dropped: never
letting the group moves increase how far a connector lies across (channels 100 × 80 mm 529 → 731 mm: rigid jumps
near corners change the nearest edge), a relaxation again after laying (1370 → 1458 mm), crowding at full weight when
choosing the place along the edge (STM32 60 × 45 mm 863 → 890 mm).
**Board use.** The stats say where the parts are, not how good a placement is. Below 15 % fill (`sparse`) most windows are
empty however well parts sit by their connections, and crowding reads 0 because the floor of a quarter of a window
decides: `place.auto` now says so and to judge by the ratsnest; `fullest_window` says how dense the parts are where
they are, on any board.
*Why:* each of these was a workaround or a wrong signal for an agent: re-sending a pin list, paying 26 KB to read an
argument it does not use, hinting every capacitor, a polarity failure that placement caused and could not see, a
metric calling a better placement worse. Remaining: session 5's board stays close to the router's limit, its
result moved by small placement changes (holding the flow-through part was needed for it to route; the other
variations tried lost one or two connections); `big_board_spread_timing` measured 63 s under load (47 s in D67, not
measured under the same conditions); the hand model takes a pair's way out as perpendicular to its pads, away from
the part's center.

### D72. Router work budget: comparisons that repeat exactly; verification inside the budget; via-aware estimate above two layers (2026-11-01)
**Measurement.** D69 could not judge its change from CI's numbers: boards ending on the wall-clock budget moved by ±1.5 points
between runs of one build (here: revD1 91.1 and 91.3 % side by side, against D69's 93.7 % on a quieter machine). `RouteOptions::work`
is a `WorkMeter`, optionally limited, counting the run's operations (`router::work`): queue pops of the grid search, gridless nodes
and rooms, pair expansions, halos, setup, and by size the post-processing and verification. A limit ends the same phases as the
time budget. Searches of a parallel batch charge a pending count joined at the batch's end, so a stop test never reads another
thread's progress and a batch stops the same way with any threads. Checked: 1/3/4/8/64 threads, several runs side by side at load
100, and local against the CI runner: identical tracks, vias, failures and work. Offered to users as `work_budget` on the four
routing commands, through the registry, with `stats.work` always reported and both budgets refused together. The time budget stays
the default: from the data, a board finishing inside it is deterministic anyway; a unit is 165–300 ns depending on the board and up
to about three times more under load, so no count means "a minute" and a work budget has no bound on time, while a limit
on the wait is what an interactive user asks for. A run the clock cut now says so (`route.budget_cut`, info, with its work).
`budget_exhausted` now also holds when a later phase stopped on the budget, not only negotiation.
**Seeds.** A repeatable result is not a representative one: one changed tie reshuffles negotiation (nrfmicro 2 to 3 failures, corne
±4 between seeds), so the comparison is several seeds at a fixed count (`CADLAB_BENCH_SEED`, CI job `corpus-route`, informative).
**Time.** Stack samples: on revD1 about 78 % of negotiation is search; on revC3 a third was the setup of each connection of large nets
(`BTreeMap`s of all access points rebuilt per connection, the reachability test over every source). Access sets are now merged as
islands join (same contents, tested against rebuilding) and the test stops at the first source that reaches a target. The estimate:
above two layers, the smaller of the distance to targets on the node's own layer and a via plus the distance to any target, so other
layers no longer look as near as the target's (consistent, so paths stay optimal). Over seeds 1–3 at 200 M units: revD1 370 → 337 failed,
mobo 13 → 3, revC3 104 → 99, tinytapeout-demo 44 → 58 (worse), buspirate5 1 → 2. Also penalizing steps against a layer's direction: more
failures (643 → 632 total but tinytapeout 59, corne 30), dropped. On two layers plain distance stays: with the via term stm32-2l went 58 → 71 vias.
**Verification.** Post-processing, verification DRC and reports ran past the budget (revD1 71–73 s on a 60 s budget). Verification runs
`drc::check_copper`, the checks that can flag a track or via (a test holds it equal to `check` for those; 9.5 → 5.7 s on revD1, both
under load), and the budget sets aside an estimate of the tail before negotiation starts (about 12 000 units per connection, two thirds of
what revD1 used, 18 M against 28 M; in time at the speed of the run's setup, at least 200 ns per unit). revD1 now ends at 63 s (60 s budget).
**Residuals.** *corne-cherry's shield connections* were no routing failure: several pads share the label `J1.S1` and the report
compared the first one's island. Each end is now the labelled item nearest to it: 5/4/7 failed instead of 8/6/11. *nrfmicro:* the crossover
via at its shallowest let the row neighbor's exit turn too early for the other pad's own exit; it now goes deep enough for that where
it can. The crossing then routes in some seeds, and with the partner's exit always placed (as D57 found) other nets fail instead; no
net gain, still 2–3 failures. *cynthion:* the 8 "no exit for 0.127 mm" pads lie inside the force pads of Kelvin shunts with net-tie groups.
Giving them exits and exempting tracks near tied pads in the DRC routed all 8, and KiCad's DRC rejected it: `shorting_items` on those same tracks,
in the original board as well. Reverted; the failure now says why and to route by hand. *Found on the way:* `failed` counts planned
connections while the pours are refilled around new copper afterwards, so `route.status` can show more unrouted than `failed` (revD1 115 and
199): the benchmark prints both now; not fixed here.
*Why:* a comparison that varies more than the change decides nothing, and a count of the run's own work removes the clock without
changing what is being measured; the default stays honest about what each budget promises.

### D73. Conformal copper in the built-in solver: Dey–Mittra cells at the uncut timestep, measured copper surface for the loss, the mesh refined for narrow diagonal tracks (2026-11-01)
**Measured first** (`tests/fdtd_conformal.rs`, staircase): a 45° microstrip (W = 1 mm, H = 0.5 mm, εr 4; field
solver 50.81 Ω, 3.071), 2–8 GHz, xy cells of 500, 250, 125, 62 µm: εeff +27…+29, +11.5…+12.9, +5.8…+7.2, about
+2.6 % (first order), Z₀ +20, −4, −4, +2 %; the same strip along x on those uniform cells without the thirds rule
stays within +1 to +6 % in εeff (+1 to +3 % from 250 µm), so the error is the diagonal's. A cylinder with a PEC wall (TM₀₁₀, TM₁₁₀, TM₀₂₀):
−2.4…−3.0 % at a/10, −0.8 % at a/40, −0.37 % at a/80. Round patch TM₁₁: −6 % at a/10. The wall's conductor Q is
−21 % at any mesh (the limit 1 − π/4: the staircase's surface is 4/π too long, so refining never helps); a 45°
strip's conductor Q is +41 %. On a routed 45° section, the default mesh of 10 GHz has 60° of phase error on W = 1 mm,
and W = 0.3 mm is an open circuit (|S₂₁| = −72 dB, no warning). Halving a cell costs 4 to 16 times the run, so
the data supports conformal cells.
**Implemented.** `fdtd::conformal` computes, for the cells a slanted polygon edge crosses, each edge's open length
and each face's open area from exact intersections of mesh lines with the polygons (copper layers and plated
barrels as prisms; union per z plane). The engine corrects H on those faces after the regular update (Dey and Mittra,
IEEE MWGWL 7, 1997): μ A H′ = −Σ l E over the open parts. A list per plane, each face written once: bit-identical for
any thread count. The energy in the end criterion uses the norm this form conserves. **Stability:** the area in the
update rises to keep a face's own frequency within a quarter of 4/Δt² (Benkler, Chavannes and Kuster, IEEE AP-54,
2006, have the idea; the bound is ours); the timestep is unchanged. A first bound of a third reached Δt² λmax = 3.97
at a/80; a quarter gives at most 3.56, below an uncut grid's 3.61. Edges next to a CPML stay whole, so every edge has
one length in all its faces (the update's symmetry); ports and elements take whole edges. **Loss:** a cut edge
carries κ, its covered fraction, of its dual loop's circulation over its covered part (power Zs κ² (∮H)² l / w), w
being the copper across its dual cell, and z edges take the outline nearest to them. **Mesh:** a diagonal track under
1.5 cells wide refines the whole mesh to half its width (`sim.mesh_refined`), through `max_cell` at the extraction,
so `src/openems` is unchanged; an explicit `max_cell` warns instead (`sim.slanted_copper_unresolved`). `conformal:
false` restores the staircase, without refinement. `Simulation::curl_curl_radius` measures Δt² λmax by power
iteration.
**After, same mesh** (staircase → conformal): 45° microstrip at 125 µm εeff +5.8…+7.2 → −0.4…+1.0 %, Z₀ −2.3…−4.1 →
−0.2…+1.5 %; at 250 µm +11.5…+12.9 → −2.8…−1.1 %, Z₀ −2.2…−4 → +2…+3.7 %; at 500 µm +27…+29 → +6.6…+8.9 %. Cylinder
at a/10 −2.4…−3.0 → −0.25…−0.87 %, a/40 −0.8 → −0.12 %. Wall Q −21 → −2.2…−4.3 %. Strip conductor Q +40.9 → −5.2 %.
Patch TM₁₁ at a/10 7.925 → 8.424 GHz (8.438 at a/20; Wolff and Knoppik 8.485 ± 2.5 %). Routed section at 10 GHz
(straight line 164.2°): default mesh 104.1° → 154.8° (refined to 0.5 mm), 0.25 mm 143.5° → 157.5°, |S₁₁| −18 → −28 dB;
W = 0.3 mm open → 141.0° at 0.15 mm (straight 154.8°, staircase on those cells 113.4°). 300 000 steps on a lossless
box of cut cells: energy −1.9 to +0.03 %, no growth. Everything together (CPML, ports, Debye, surface impedance,
lumped elements, one on a cut edge) converges and is bit-identical on 1 and 3 threads.
**Cost:** per step about 0–4 % on the microstrip (20 against 20 s at 250 µm, 50 against 52 s at 125 µm), setup
negligible. These timings come from a heavily loaded machine at 3 threads and are only indicative. Reaching the same
εeff with the staircase needs a mesh at least four times finer in x and y, 16 times the cells or more.
*Why:* first-order errors that refinement barely reduces, and a loss error it does not reduce at all, are what a
conformal method exists for, and the update is only a list of faces. Dey–Mittra rather than cell enlargement because
raising an area is local and keeps the update's symmetric form, hence its energy, with no neighbour bookkeeping; the
timestep stays what the mesh gives, which matters since one sliver would otherwise slow the whole board. Leaving
axis-aligned copper alone keeps the thirds rule and every earlier validation. Refining the mesh, not only warning,
because an open circuit is a wrong answer an agent cannot recognize, and more cells are at worst `sim.too_many_cells`.
*Left:* κ is a modelling choice validated on one strip mesh and on walls, not on `sheet` copper; probes on a lossy
diagonal line read α within ±30 % (hence the resonator). A slanted strip still needs about two cells across, a sheet's
edge singularity stays first order, the stability bound is checked on the geometries tested rather than proved, and
dielectric outlines still staircase. The refined mesh is `sim.sparams`'s own: `export.openems` needs the same
`max_cell` to compare.
*Lumped parts.* The first version placed elements on the copper as cut. A track ending at a pad's center has a round
end that passes a 0603 pad's inner edge by 50 µm; that made the edges next to the gap partial on every column but the
one at the end's tip, so the element kept 1 column of 3 at 0.5 mm and 1 of 5 at 0.25 mm. `sim_sparams_parts_validation`
caught it: a series 1 pF read up to +0.036 in |S21| (staircase +0.014), the same for 1 pF + 1 nH + 1 Ω, and a resistor
hid it; the error did not close with the mesh because the tip's width does not depend on it. Not the gap's capacitance,
as the size first suggested: with the part left out the gap holds only 0.007–0.009 pF. Parts are now placed on the
copper as voxelized, and cut edges between a pad's center and the element become whole copper, since a terminal
has to reach its element. Same case, staircase and conformal: +0.0099 and +0.0099 at 0.5 mm, +0.0118 and +0.0117 at
0.25 mm, on 3 and 5 columns both ways (`columns` in the output; a 0 Ω short now also clears its edges' cuts).

### D74. openEMS: the absorbing cells beyond the region, via barrels as node columns, board parts as lumped elements (2026-11-02)
Closes what D68 left open. Everything below comes from running openEMS as a black box (its CLI on XML written from
the observable format, a build without the Python modules, one at v0.37.0-32; the driver at CI's pinned revision for
the S-parameters); neither openEMS's nor CSXCAD's source was read. Full table: ELECTRICAL.md, "What makes openEMS
diverge".
**The divergence.** D68's "needs the GND planes" was a symptom. `PML_8` is the outermost eight *cells*; on the via
board's fine mesh that was 61 % of the region's width in y, and the outline lay one 0.30 mm cell from the wall, because
a 4 mm margin around y = 4 ends 0.30 mm past an 8 mm board. The trigger is two planes with dielectric between them
ending together on the last interior line of the y sides: +5 dB per 1000 steps, then NaN. Moved 2, 3, 5 or 8 cells
in, onto the wall, or one cell from the wall on x (a 1.18 mm outer cell), it decays over 250 000 steps; so it does
with one plane, no dielectric, or εr 1. Tracks, vias, copper thickness, lossy materials and the pulse's DC content
(−20 to −163 dB) make no difference, and the rate per unit time does not change with half the timestep, so this is
neither D68's materials nor a Courant limit. `sim.sparams`'s CPML on the same region is stable; it has a complex
frequency shift, but openEMS's layer's internals are not read, so that is not established as the reason. **Why** this
position is unstable is not established either: the runs identify the trigger, not the mechanism.
**The fix.** `pml_outside` (default true in `export.openems`, false in `sim.sparams`): the layer's cells are added
beyond the region's x and y sides, copper and dielectrics reaching a side run through them to the wall
(`openems::reach_wall`), what ends inside the region stays out. On the same board the run now reaches its end
criterion by 28 000 steps and keeps decaying over 219 000; with `pml_outside: false` it still diverges, and warns
`openems.edge_near_boundary`.
**Mur.** D68's "any region with a dielectric" was the same position, not the dielectric: Mur diverges with a
dielectric's edge 1, 2 or 3 cells from the wall on y (planes not needed), and stays bounded on the wall or 4, 6, 8
cells from it (200–250 000 steps; 40 000 steps had called 2 and 3 safe). So Mur gets eight air cells beyond the region
the same way, and works. It keeps one limit, warned as `openems.mur_static_field`: with `f_min` 0 the pulse's DC part
stays as a static field, and the energy stalled 27 dB below its peak.
**Via barrels were open.** With the divergence gone the via read |S21| −69 dB, |S11| 0 dB: every plated barrel between
mesh lines was missing from the grid (no mesh edge inside the cylinder), since D54, unseen because no run had reached a
result. A line `Box` at the barrel's nearest node column closes it, as `sim.sparams` has done since D59; it must be on
mesh coordinates, since a degenerate line between them made no difference. |S21| then agrees within 0.03 dB.
**Parts.** D65's parts through the same selection (`fdtd::parts::candidates`) as `LumpedElement`s over the gap between
the pads (`boxes`: on the mesh, not on voxels), `parts` as in `sim.sparams`: R alone a plain element, with a reactance
`LEtype="1"`, 0 Ω metal. Within 0.009 of `sim.sparams` and 0.011 of theory, 0.5–4.5 GHz. `openems.part_in_pml` warns
when one lies in the layer with `pml_outside: false`: another project's black-box notes report such elements
diverging; not reproduced here, so the warning is a precaution.
**Guard.** A diverged run ends as if converged ("end-criteria reached (-nan dB)"), so the driver exits with
`openems.diverged` on non-finite or |S| > 10 results instead of writing a Touchstone file.
*Why:* moving the layer rather than warning fixes the default path for every board without a mesh rule fitted to one
trigger: the export does not need to know which positions are unstable, only to keep edges out of the layer, and a
layer of cells that hold only air or a uniform cross-section is what a PML is meant to absorb. It costs cells (177 000
against 91 000 on this board); a warning alone would have left the default run diverging. `sim.sparams` keeps its
default so its validated results (D59–D68) do not move; the comparison passes the option to both. The barrels are a
node column, not a mesh line, so the mesh `sim.sparams` shares does not change. Also tried and rejected: mesh lines at
the via centers (they change the shared mesh, and that run read |V2/V1| 0.09–0.12 with slow growth when cut off at
46 000 steps, not explained). Treating "structures out of the PML" as sufficient in general is not claimed: a small
model with PEC walls on x and z diverged with a vacuum-only PML (unexplained, and not a configuration the export writes).

### D75. Fab zip archives written with fstool; MSRV 1.95 (2026-11-04)
The fab archive (D12 export) is written by `fstool`'s zip writer (the maintainer's own crate, MIT, features
`archive` + `gzip` only) instead of the `zip` crate, and the fab tests read it back with fstool's reader. Members are
deflated with every field fixed: 1980-01-01 mtime/atime, uid/gid 0, mode 0644, in the given order, so the same files
give the same bytes on any OS and timezone. The bytes differ from archives written before this change (different
deflate encoder and extras); file contents and names are unchanged. fstool needs Rust 1.95, so the MSRV moves from
1.89 to 1.95. The writer currently needs a fixed-size in-memory device (its growable one isn't usable yet), so
`fab::export::zip` sizes it from the contents and cuts at the reported archive length.
*Why:* one less third-party dependency on the export path (and its extraction-side advisories), and archive
handling stays in a crate the project's maintainer controls.

### D76. Net exceptions in keep-outs (2026-11-13)
The item session 6 left open (D70): a keep-out under an antenna or around an RF feed must let the feed net through,
and the agent had to draw two rectangles, one each side of the track. A keep-out now carries `allow_nets`: the nets
it does not apply to.
**Meaning.** An exception covers the copper of the net: its tracks, vias and pours, each where the keep-out forbids
that kind. Footprints stay forbidden (a courtyard has no single net), netless copper is never excepted, and an
exception on a keep-out that forbids no copper is refused (`keepout.nothing_excepted`) rather than stored as a
setting that does nothing. It is a list of nets, not of net classes or patterns: the cases seen name one or two
nets, and a class can be added later without changing what is stored for nets.
**Model.** `Keepout::allow_nets`: net names, sorted, without duplicates, left out of the file when empty. Boards
without exceptions are saved byte for byte as before, so the schema version stays 1 and there is no migration (as
for the optional fields added before, D40). Net renames and merges (`move_net`) rename the entries. A name
whose net no longer exists excepts nothing and is reported by the DRC (`drc.keepout_net`, a warning), not removed
behind the user's back.
**Commands.** `keepout.add --allow-nets`, and `keepout.set` (`allow_nets` adds, `forbid_nets` removes: both are
lists, so neither needs a way to say "empty" on the command line); `keepout.list` and every summary show
`except net(s) ...`. Nets must exist (`net.not_found` with suggestions).
**Consumers.** DRC: `drc.keepout` skips tracks and vias of excepted nets. Zone fill: a `no_pours` keep-out is not
subtracted from the pour of an excepted net. Router: a keep-out obstacle carries the excepted net ids
(`Obstacle::except`), and every question "is this in the way of net N" skips it for them: the exact checks, the
gridless rooms and refinement, push-and-shove (so `route.track` refuses any other net and never shoves a neighbour
into the keep-out). The grid's static maps are per rule profile, so their per-point state grew from "free / one
net / blocked" to "free / one net / a set of nets / blocked" (`grid::SET`, `Statics::sets`), combined by
intersection; this is exact for any number of excepted nets and overlapping keep-outs, where giving excepted nets
profiles of their own would have collided with neck-down and pair profiles, which are shared by key. A differential
pair's centerline net is excepted only where both of its nets are. Failures at a keep-out hint at the exception.
Placement is unchanged (footprints are not excepted).
**Formats without a per-net exception** (Specctra DSN, IDF 3.0, IDX, KiCad rule areas): the keep-out is written as
it is, without the exception, and the export warns `export.keepout_net_exception` naming the keep-out and its nets;
IDX also names the exception in the item's description, and the KiCad export lists it in its `warnings`. Stricter,
never looser, the rule D47 took for IDF's layers: a reader that honors the file can never put copper where cadlab
forbids it, at the price of an external router leaving the excepted nets unrouted there (route them in cadlab
first: existing tracks are written as wiring) and of KiCad's DRC reporting their copper. The other way round,
dropping the keep-out for the kinds it excepts, would let a freerouting session route any net through an antenna
keep-out. Imports never create an exception: none of the formats read holds one, so a cadlab → KiCad → cadlab
round trip loses it. IPC-2581, Gerber and the renders carry no keep-outs, so nothing changes there.
**Not done.** The KiCad export could express the exception exactly with a custom rule in the `.kicad_dru`
(`disallow` with an `intersectsArea` and net name condition, as the export already writes for cadlab's custom
rules) on a rule area that forbids no copper, and the import could read that pair back. It needs KiCad to confirm
the semantics (rule area layers, pours judged by outline or by fill), and `kicad-cli` only runs in CI, so it is
left for a change that can add an oracle case. `track.add` and `via.add` place what they are told, as before; the
DRC reports copper they put in a keep-out.
*Why:* the exception is the designer's intent ("nothing here but the feed"), and one keep-out states it where two
rectangles only approximated it: they left the strip under the track open to every net and had to be redrawn
whenever the track moved. Carrying it through the DRC, the fill and every router stage keeps the rule the project
has had since M5, that the router returns nothing the DRC would flag.

### D77. Fab profiles re-checked: unverified values closed only from the fab's own pages and files (2026-11-13)
Every built-in fab profile was read again against the fab's own pages, and the files those pages publish (PCBWay's
sample centroid file, NextPCB's and Elecrow's BOM templates, the CPL files shown in Eurocircuits' upload guide, the
Elecrow PDFs). A field left `unverified` only when its value was read there in that pass; `verified_at` moved to
2026-11-13 for the ten fab profiles (`generic` is cadlab's own rule set and keeps 2026-10-04).

Closed (now cited):
- **JLCPCB** `cpl.origin = board`: its assembly files checklist asks for one common origin for the Gerber and the
  centroid files. `part_classes` (basic, extended): its assembly FAQ.
- **PCBWay** `cpl.columns`: the headers of its sample centroid file (`Designator, Footprint, Mid X, Mid Y, TB,
  Rotation, Comment`; its `Ref X/Y` and `Pad X/Y` columns are left out). `cpl.origin`: its KiCad position file guide
  puts the origin at the bottom left corner of the board frame, so the value changed from `board` to
  `outline_lower_left`.
- **OSH Park** `drill_format`: its KiCad Gerber guide states that drill files in inches and in millimeters both
  work (decimal format, absolute origin).
- **Eurocircuits** `cpl.columns`: headers and side names of the CPL file shown in its upload guide (`Designator,
  Comment, Layer, Footprint, Center-X(mm), Center-Y(mm), Rotation`; `TopLayer`/`BottomLayer`).
- **NextPCB** `bom.columns`: its BOM template (`Manufacturer Part Number, Qty, Designator, Customer Supply,
  Customer Remark`), which replaces the three guessed headers.
- **Elecrow** `bom.columns`: its BOM template has the stored headers, and explains `*Original` (Y: original brand
  required, N: substitutes accepted), which cadlab leaves empty for the user.

Corrected: **JLCPCB** `min_package` 0201 → 01005 (Standard PCBA row of the assembly capabilities table), so
`fab.check` no longer reports 01005 parts as `fab.package_size` there. No other drift was found: the stored process
values that were compared with the pages read in this pass were unchanged.

Still unverified, with what was looked at:
- **JLCPCB**: 1-layer copper weights and thickness per layer count (the capabilities table has no such rows; it only
  says 2.5 mm and above need 12+ layers); `names` (the naming article has no paste layers and inner layers only to
  `.G3L`; the KiCad 9 article names `.GTP` but not `.GBP`); `archive` (one zip or rar, flatness not stated).
- **PCBWay**: copper weights and drill range (capabilities page: up to 8 oz, 0.15–6.0 mm; tolerances page: 1–3 oz,
  0.2–6.3 mm; the stricter values stay), multilayer thickness (not per layer count), and the new `cpl.side_names`
  (the sample only has `T` parts; the requirements page says "Top or Bottom"). The sample is in mils with a unit
  suffix while the help page asks for mm; cadlab writes plain millimeters.
- **Aisler** `drill_format`: the Gerber page still asks for inches 2:4 and does not mention metric files.
- **Eurocircuits**: `hole_to_hole` (0.25 mm between tool sizes, a finished-size value is derived), `finishes`
  (immersion silver is pooling in one list and non-pooling in the other), multilayer `thickness` (1.55 mm is listed
  for 6 layers and up), `bom.columns` (the BOM shown is an EAGLE parts list without the required MPN column, so it
  was not adopted), `cpl.origin` (units and origin are not stated). Its classification table is an image and was not
  read again; the ring and hole values were confirmed on the service pages.
- **Seeed**: outer copper (1–3 oz against 0.035–0.07 mm), inner spacing, 6-layer thickness (listed for "6 and 8
  layers" while layers are 1–6), `names` (no paste or NPTH names), CPL columns and origin (not published). The
  order page is rendered by script and was not read; the specification article was.
- **NextPCB**: `min_drill` (0.15 mm only up to 1.2 mm thick, PTH from 0.2 mm), thickness and odd layer counts, CPL
  columns and origin (the export guide only says KiCad's defaults are fine).
- **PCBgogo**: `min_npth` (0.3 and 0.5 mm rows), `min_silk_height` (0.75 mm with a 0.8 mm note), odd layer counts,
  double-sided assembly (implied only), CPL columns, bottom side name and origin (the sample is top-side Altium text
  in mils; the page says to confirm the origin before release).
- **ALLPCB**: nothing listed as unverified; thickness options, a minimum size and a BOM/CPL layout are still not
  published, so they stay left out.
- **Elecrow**: track/space (6 mil on the order page, 0.15 mm in the PDF), `min_drill` (0.25 mm vias, 0.3 mm holes,
  0.3 mm in the comparison table), 6/8-layer rings, `names` (inner names only for 4 layers), CPL columns and origin.
- JLCPCB's impedance stackups (read 2026-10-15) were not read again.

*Why:* the unverified list is only useful if leaving it means something. Sample files and templates a fab publishes
are as authoritative as its tables and are the only place the assembly layouts are written down; where a fab
publishes nothing, "still unverified" with the pages that were read is the honest state, and the stricter stored
value keeps `fab.check` on the safe side.

### D78. Footprint tables compared with what can be read of IPC-7351B; BGA lands from the land table (2026-11-13)
IPC-7351B is paywalled, so the flat-lead, flat-lug, molded-body and MELF fillet rows and the BGA land table were
compared with published reproductions, read on this date. No GPL project or KiCad library was consulted. Sources
(named per row in `src/landpattern/ipc.rs`, summarized in PARTS.md):
IPC's own table of contents of IPC-7351B (table numbers and titles only); page images of Tables 3-2, 3-3, 3-5, 3-6
(IPC-7351B) and of the inward flat ribbon L table (earlier IPC-7351), posted on the PCB Libraries forum; PCB
Libraries' "Library Expert Surface Mount Families" of 2016-02 (archive.org), the tables of the tool that was IPC's
calculator; T. Hausherr's blog series "PCB Design Perfection Starts in the CAD Library" (Mentor Graphics,
2010–2011, archive.org); TI SPRACA4; the KEMET T491 datasheet; Diodes Inc. AP02001 Rev. 81.
**Result.** No fillet value changed.
- *Molded body* (0.8/0.5/0.2 outer, 0.25/0.15/0.07 inner, side 0.01/−0.05/−0.10): checked. The page image has
  these numbers, and KEMET's level A/B/C lands for the T491 follow from them to 0.02 mm, the negative side goals
  included (the differences between KEMET's land widths are 0.12 and 0.10 mm, twice the goal steps).
- *MELF*, *flat lead* and *flat lug* (the gull-wing numbers): equal to the 2016 document, one source. That
  document prints 0.00 where IPC has a negative goal (visible on the chip, fine-pitch gull-wing and molded rows
  that the page images also give) and 0.12 where IPC has a 0.1 mm courtyard, so it does not confirm the flat-lead
  Least side goal (−0.05) or Least courtyard (0.10): those two cells stay unchecked, with the values they had.
- *BGA lands, collapsing balls*: **changed.** The generator reduced the ball by 25/20/15 % and rounded to 0.01 mm
  (0.4 mm ball → 0.32); IPC-7351B Table 14-5 lists nominal lands rounded to 0.05 mm (0.4 → 0.30, 0.35 → 0.30,
  0.75 → 0.55). TI and Hausherr print the same eleven rows from 0.75 to 0.20 mm; those are now the lands, and ball
  sizes between the rows take their group's reduction rounded the same way. The 0.17 and 0.15 mm rows have one
  source. The land no longer follows the `rounding` option. The two sources disagree on which column to use
  (TI: the nominal land; Hausherr: IPC's calculator takes the upper end of the "land variation"); the nominal is used.
- *BGA courtyard* (2.0/1.0/0.5 mm): unchecked and unchanged. Only Hausherr gives the values, and there the level
  follows the ball size, not a density choice. Non-collapsing lands (Table 14-6) are not generated: one source.
*Why not more:* a row counts as checked only with two independent sources; one vendor's transcription is recorded
as such rather than promoted. Changing the BGA courtyard to follow the ball size, or the flat-lead cells to the
2016 document's values, would replace one unconfirmed number with another.
**Consequences.** Footprints with 0.4 mm balls get 0.30 mm lands; `bga_fanout_is_drc_clean` routes them at the
default effort (`low` left 1 of 64 connections open). The Diodes layouts used in the new tests are not the Nominal
calculation (between Nominal and Most for MELF and SOD-123F; DPAK pads up to 1 mm shorter), so they bound the
generated patterns without confirming a table. Seen on the way, not changed: IPC-7351B names BGA land patterns
without the M/N/L suffix, and the built-in DPAK lead pitch is 2.29 mm (JEDEC 2.286).

### D79. IPC rule values confirmed on secondary sources and kept in a data file; the IPC-2152 fit is the universal chart (2026-11-13)
The IPC numbers behind `board.rules --preset ipc2|ipc3` (D26) were marked unverified. They were checked on
2026-11-13 against sources that can be read freely; the standards themselves stay unread (paywalled). A value
counts as **confirmed** when at least two independent sources attributing it to the standard were read and
agree; pages that say otherwise are recorded as dissent next to it. Results (table with sources in
docs/BOARD.md, "Rule presets"):
- IPC-6012 Table 3-9, class 3 ring 0.050 mm external / 0.025 mm internal and class 2 breakout to 90°: confirmed,
  unchanged.
- IPC-2221A/B Table 9-1 fabrication allowance: **corrected** to 0.4 / 0.25 / 0.2 mm for levels A / B / C. The
  0.6 / 0.5 / 0.4 mm written before appears in one article without table or revision; five other sources,
  including the page cited before, give the corrected set. Table 9-2 (0.05 / 0.025 mm design ring): confirmed.
- IPC-2221B Table 6-1 up to 15 V: values confirmed, one **label corrected**: 0.13 mm is A5/A6 (conformal coating,
  uncoated component leads), permanent polymer coating (B4) is 0.05 mm.

The values and the presets move from Rust into `rule-presets/ipc.toml` (`model::rule_presets`): references with
standard, value, status, sources and dissent, and per preset the rule fields that differ from `Rules::default`
with the references each is built from. `Rules::preset` reads it; `board.rules --preset` warns
`board.rules_unverified` for a preset value resting on an unverified reference (none today; the code was
emitted before only for fab-derived values). The generic fab profile keeps its limits, which are cadlab's own
choices and so carry no `unverified` entries; its header now says how they sit against the IPC values.

The `ipc3` preset keeps its numbers (ring 0.25 mm, via 0.3 / 0.8 mm) with a corrected derivation: class 3
external minimum plus half the **level A** allowance (0.05 + 0.4 / 2). It had been described as level C with
the wrong allowance; the true level C gives 0.15 mm, level B 0.175 mm.

`current.width`'s IPC-2152 fit was described as the standard's conservative chart. It is a fit of the
**universal** chart (Figure 5-2, the 3 oz baseline); the conservative chart is the IPC-2221 internal chart
(Jouppi's IPC paper, Olson's tutorial) and asks for a third to a half more copper at the two points with published readings. Error band of the fit: +2.6 %
and −1.3 % against the two published readings of the one point found (10 A, 20 °C: 500 and about 520 mil²);
unknown elsewhere, since no table of chart points is published. The IPC-2221 internal formula is within 2.0 %
and 4.4 % of the two published readings of the conservative chart. Code unchanged; docs, help text and unit
tests state this (docs/ELECTRICAL.md).
*Why:* a preset is advice the user cannot check without buying the standards, so each number needs a trail that
can be re-read and a status that does not claim more than was done. Keeping `ipc3` at level A avoids silently
loosening a class 3 rule on existing users and is the standard's preferred producibility level; a tighter
level is one line in the data file or an explicit `min_annular_ring`. `ipc2`'s 0.13 mm ring stays cadlab's own
choice below the IPC-2221 level C design land (0.15 mm), relying on class 2 accepting breakout: changing the
default rule set of every new project was left out of a verification pass. Applying the IPC-2152 modifiers
(copper weight, board thickness, planes) was left out for the same reason: their charts are published only as
images, and a fit of them could not be checked against numbers.

### D80. Router and pours: fills are places to connect to, the run refills them for its routing and negotiates again for what it cut off, and the count is the board's (2026-11-13)
**Measured first.** The corpus re-route at a 200 M work budget (seeds 1–3, D72) on the version before: 210/202/201 connections
failed, and 480/468/457 open on the routed boards with their pours filled again (`route.status`, the DRC). The difference was
known (D72, "not fixed here") and it was most of what was left: buspirate5-rev10 reported 1 failed with 66 open, lumenpnp-mobo
3 and 69, glasgow-revD1 115 and 199. Cause: the plan reads connectivity from the board as given, fills included, so pads a
pour joins need no connection; the re-route starts from an empty board, where every surface pour is whole, then routes
tracks across it; the pour is filled again around them and the pads inside the cuts are open, uncounted. The reported
completion (corne-cherry 98.8 %) was of the planned connections only.
**Decision.** (1) *Fills are places to connect to* (`router::pours`): each fill island of a net is rasterized into the grid
cells where a track of the net may end on its copper, and a connection whose far side holds fills may end on those within
4 mm of its near end. (2) *Pour rounds*: on a legal routing (after legalization) the run computes the fills for its own
wires, plans again every net whose fills no longer join an island of its plan or that has a routed connection left open,
and negotiates again with those nets to route; up to 3 rounds before the retries and via minimization and 2 after. A round
that leaves more connections open than before it is undone. A first negotiation still going at 55 % of the budget ends
there when the routing as it stands cuts off one pad per 100 connections or more. (3) *The count is the board's*: a net with
more ratsnest lines open on the verified board than failed connections reports the difference as failed, with a reason and
hints, so `stats.failed` and `completion` no longer undercount.
**Result** (same budget and seeds, zero DRC errors other than unrouted before and after, identical for 1 and 4 threads):
open connections 480/468/457 → 254/251/297 (−43 %). Per board, seeds 1/2/3: buspirate5-rev10 66/63/64 → 4/0/7,
lumenpnp-mobo 69/62/61 → 8/14/14, lumenpnp-ringlight 2/2/2 → 0/0/0, corne-cherry 13/12/17 → 4/5/5, glasgow-revD1
199/197/184 → 137/145/170, cynthion 55/52/53 → 32/27/33, tinytapeout-demo 37/41/37 → 29/23/28, sweep-v2.2 5/4/6 → 3/3/3;
glasgow-revC3 32/33/30 → 35/32/34 (worse by 6 over the seeds: its pours are inner planes that tracks hardly cut, so the
checks cost budget and gain nothing); nrfmicro and the two small buspirate boards unchanged. Reported failures rise
(210/202/201 → 265/260/313) because they now include what was hidden. Generated benchmark: boards without zones route
exactly as before; qfp-qfn-4l (inner-layer pours) 125 → 128 vias and 1984.6 → 1934.5 mm against HEAD, 100 % and zero DRC
errors, inside the golden's tolerance (128 vias, 2000.0 mm). The goldens were not blessed: stm32-4l, fine-4l and bga144-4l
already differed from them within tolerance before this change (54/53 vias, 103/111, 154/146), and that drift is not this
change's to record.
**Exit criteria, as measured.** *M5* (typical 2- and 4-layer hobby boards to 100 % with zero DRC errors): met by the
generated boards and by three of the five small corpus boards; not by nrfmicro (2–3 of 85 open: the USB-C D+/D− crossing,
which the original board routes, so the router's miss). sweep-v2.2's three open connections are open in the original board
as well (`CADLAB_BENCH_ORIGINAL=1`: `gnd`, `raw`, `vcc` between the halves of the split keyboard; no other corpus board has
any), a residual of the design. *M6* (equal or better completion than freerouting on the benchmark corpus): met on the
generated benchmark suite by the recorded comparison; on the open-source corpus freerouting has never been run, and cadlab
leaves connections open on nine of the twelve boards (2 to 170), six of which end on the budget. Neither line is marked done.
**Tried and dropped.** Routing the cut-off nets again without sharing after legalization (buspirate5 31 failed: their
neighbors do not move). A follow-up run of the whole router on the routed board (buspirate5 15 open in 63 s: setup per round, and no
negotiation with the nets in the way). Ending every first negotiation at 55 % of the budget (revC3 42 open instead
of 35). Planning the cut-off nets again inside the running negotiation (revD1 416 open). Negotiating again for everything
legalization left failing (worse on four of six boards).
**Known residuals.** cynthion's 8 Kelvin sense pads (infeasible under its rules, D72); sweep's 3; the nrfmicro crossing;
6 pads still cut off from a pour in seed 1, reported as such; fine-pitch pads with no exit (tinytapeout-demo 7, mobo 1,
corne-cherry 1); the rest is congestion at the budget (both glasgow boards, cynthion, tinytapeout-demo). A net is planned
again whole, and the fills checked are those before optimization.
*Why:* a completion figure that the board itself contradicts cannot close an exit criterion, and a pour is copper the
router changes by routing, so it has to look at the result; counting from the refilled board makes every later number
comparable with what the user sees.

### D81. Milestone exits: an exit is done when a named test that was run shows it (2026-11-13)
Every roadmap checkbox was ticked while only M0 and M2 had a dated exit. The audit went through the others (M5 and
M6, the router's, are handled separately) and wrote on each **Exit:** line the test that shows it, or what is missing.
- **M1, done:** `tests/sourcing.rs` `ldo_from_requirement_to_bom`, which existed but stopped at the footprint's name;
  it now ends at a concrete BOM line in stock and at the saved part's symbol, footprint and one-to-one pin↔pad map.
- **M3, done:** `tests/schematic_layout.rs` `attiny_schematic_png_and_svg_are_readable`. "Readable" is measured: the
  netlist read back from the drawn sheet (wires, labels and power symbols by name) equals the circuit's, nothing
  overlaps, everything is inside the frame, the PNG has ink at every symbol and label, the output repeats.
- **M4, not done:** the demo board is `examples/demo-board/board.jsonl` (session 1's board as a batch) and
  `tests/demo_board.rs` shows cadlab DRC and both bundles from one unchanged project. The KiCad DRC oracle on it is
  written and has never run (no `kicad-cli` outside CI), and the fabs' online checks need a person: checklist and
  result table in MANUFACTURING.md, "Demo board".
- **M7, M8, M9** had no exit; each now states what its items add up to and names existing tests. M7 is done on KiCad
  files cadlab wrote and on fixtures, and rests on CI's corpus job for third-party projects. M9 gained an open item:
  FreeCAD and the IPC-2581 and IDX schema validations are optional oracles no automatic run includes.
No defect in the library came out of the audit; no check was loosened.

Choices made on the way:
- **The demo board is a batch file, not Rust.** The file a user runs with `cadlab batch` is the file the tests run,
  so the documented command sequence cannot drift from what is tested, and it goes through the registry like any
  session. It differs from session 1's replay in three ways: both fabs are `targets`; the rules are cadlab's
  defaults instead of `board.rules --fab jlcpcb`, because rules derived from one fab would put that fab's numbers in
  a project meant for two (D12); and the USB-C shell pads are a grounded SHIELD pin, as `part.unmapped_shield` asks.
  `tests/agent_session.rs` stays as it is: it replays what the agent typed.
- **No golden bundle.** The archives are compared between two builds (registry and CLI), not with checked-in
  hashes: they follow the router's output, which is still being worked on, and a golden file would fail on every
  router improvement without saying anything about the exit. `route.all` runs under a work budget (D72) so that the
  two builds, and any machine, give the same bytes.
- **The oracle test is stricter than the cross-check's allowlist:** it drops the allowances for dangling tracks and
  vias, which exist for boards with deliberate stubs. If KiCad finds one on an autorouted board, that is a result.
- **The schematic's golden SVG leaves out the title block,** whose stroke text carries the crate version; the test
  checks separately that the command's file is the full scene.
- **"Done" needs a run.** A test that exists but could not run here (oracles, corpus) is named with where it runs,
  and the exit is not marked done on its account.
*Why:* a ticked list says what was built, an exit says what works end to end, and the second is only worth writing
down with the evidence next to it. Where the evidence is partly out of a program's reach (a fab's upload page), the
line says so and says who has to do what, instead of rounding up.


### D82. Silkscreen: edge and overlap rules, reference designators arranged and kept in the board (2026-11-13)
The first KiCad DRC of the M4 demo board in CI (KiCad 10) failed: the reference designators of J1 and J3, the
connectors at the top edge, were printed across the board edge, and about a dozen more sat on other silkscreen
(allowed until then as "a rule cadlab does not have"). cadlab printed every designator centered above its
courtyard, whatever was there, and its DRC said nothing: a real defect in what a fab prints.
- **Two DRC rules, values in `board.rules`:** `drc.silk_edge` (`silk_to_edge`: any silkscreen to the outline and
  cutouts) and `drc.silk_overlap` (`silk_clearance`: a text to the silkscreen of another owner). Both 0.15 mm by
  default, KiCad's default silkscreen clearance, and left out of saved rules at that value, so existing projects
  load unchanged and get the checks. Scope of the second: texts only (designators and board texts) against other
  footprints' drawings and designators and board drawings and texts; a designator on its own footprint's drawing is
  the footprint's business, and drawings against drawings are what courtyards already keep apart.
- **Warnings, not errors,** like `drc.silk_over_pad` and like KiCad's: the board works either way, the fab cuts or
  overprints the ink. Waivable (an edge connector drawn to the edge).
- **Zero means "may touch", not "unchecked".** The plan was for zero to switch a check off, as a zero limit does
  in fab checks (`drc::check_limits`). In `drc.run`, though, a zero `silk_to_pad` still reports overlaps (D46),
  and a zero KiCad silkscreen clearance still reports crossing and overlapping, which is what many imported
  boards carry. Unchecked at zero would leave every such KiCad finding without a counterpart. So zero only drops
  the distance; to drop a check, waive its code.
- **No fab profile field.** Profiles carry values with a source on the fab's own page. The fabs' pages were not
  searched for a silkscreen-to-edge or silkscreen-to-silkscreen distance in this change, and a number without a
  source is what profiles must not hold; a field can be added when a page is found. `board.rules --fab` leaves the two rules alone and `fab.check` does not check them. A
  designator given a height under the fab's `min_silk_height` is reported (`fab.silk_height`).
- **The designator's place is in the board** (`PlacedFootprint.ref_text`: offset from the origin in board axes,
  rotation, height, hidden, set by hand or not), unset for the default above the courtyard, so no migration and
  untouched boards save as before. One function (`board::silk::ref_text`) answers every output: the Gerber
  legend, IPC-2581, the renders (which used to draw the designator on the footprint origin, sized to the part,
  unlike the legend), the 3D board faces and the KiCad export, whose `Reference` field now carries rotation,
  stroke and `hide`. An offset in board axes follows `place.move`; turning or flipping a part drops an arranged
  position, never one set by hand.
- **Arranging is a pass over stored data, not something each output recomputes:** `silk.arrange`, run by
  `place.auto` after placing, not by `place.set`/`move` (the DRC then says what is clipped and names the command).
  Positions are tried in a fixed order around the part, then inside it, the text counted as a box; the texts with
  the fewest free positions choose first, and one text may make room for another. Details in BOARD.md. Three
  rounds: clearances plus 0.1 mm and "no nearer to another part than to its own"; without the margin; then also
  beside or beyond a neighbour, up to 3.3 mm away. Then hidden, with a warning: never printed clipped.
- **Size stays 1 mm.** A smaller text would fit more often, but the smallest height a fab prints is the fab's
  (JLCPCB 1.0 mm), and the board must not depend on one (D12). `silk.set --size` is the designer's call.
- **A far designator rather than none.** The first version stopped at 1.3 mm and hid what did not fit: one
  designator on the demo board (R6, between an LED and the MCU's pins), up to six on the session boards, among
  them an IC. With the third round reaching 3.3 mm, none is hidden on those five boards; such a text stands
  next to its neighbour's (the demo's "D2" over "R6", left of the pair), as a person would write them.
- **The KiCad import keeps each `Reference` field** (position, angle, height, hidden; set by hand as far as cadlab
  is concerned). Before, imported boards printed designators above courtyards, not where their designer put
  them, and KiCad's silkscreen findings on them could not be paired. A field at cadlab's default place leaves
  `ref_text` unset, so cadlab's own exports of unarranged boards round-trip exactly; an arranged text comes back as
  one set by hand (a KiCad board does not say who placed a text). KiCad's single `min_silk_clearance` becomes all
  three silkscreen rules; the export writes `silk_clearance` back (KiCad applies it between silkscreen items and
  to the edge, never to copper), no longer `silk_to_pad`.
- **The comparison pairs the findings:** `silk_edge_clearance` → `drc.silk_edge` and `silk_overlap` →
  `drc.silk_overlap` in `TYPE_MAP`, per-segment KiCad findings folded as for `silk_over_copper`, measured values not
  compared (each tool's own font); the one-sided allowances for both types are removed, from the cross-check, the
  demo board's oracle test and the corpus.
- **`tools/list` budget 42 → 44 KB:** the `silk` group (four commands) brought it to 43.3 KB.

What supports the fix without KiCad: run on the demo board's placement with the designators back above their
courtyards, the new rules report the two designators across the edge and the overlaps CI's KiCad run listed, with
the same 0.10 mm where KiCad gave a distance; after `place.auto` they report nothing, and no designator's box is
within 0.15 mm of an edge or of other silkscreen (`reference_designators_are_legible`, and the same check on the
boards of agent sessions 1, 3, 5 and 6).
**Not verified:** `kicad-cli` was not available, so neither the demo board's oracle test nor the cross-check nor
the corpus ran with this change; KiCad's glyph widths are unknown here, so a designator pair it measures closer
than cadlab's box allows for remains possible. The corpus manifest was not re-measured: KiCad's findings on
`Value` fields and user texts (not imported) and on one footprint's drawing over another's have no counterpart.
Not done: reference designators are still not checked against pads by `drc.silk_over_pad` (the legend writer
clips them there, and KiCad's finding on them stays an allowance); a `silk_to_edge` different from
`silk_clearance` is not exported to KiCad; other footprint texts are not modeled.
*Why:* a legend is read by the person holding the board, so a clipped or overprinted designator is a defect even
though nothing electrical depends on it, and the tool that placed the parts is the one that knows where the free
room is. Storing the result keeps every output and both DRCs talking about the same text, and leaves the last
word to whoever sets one by hand.

### D83. Silkscreen DRC against KiCad on the corpus: drawings against drawings, texts drawn as KiCad places them, the font as the measured residual (2026-11-13)
D82's two rules ran against KiCad for the first time in CI and the corpus job failed (run 43408253002): 518
differences, both ways. KiCad alone reported silkscreen drawings over each other and at the edge; cadlab alone
reported hundreds of board texts over other silkscreen. Each cause was found from the data (KiCad's reports
replayed against cadlab's findings, then probes run by `kicad-cli` in the oracle job, `tests/kicad_silk_oracle.rs`),
none from KiCad's sources.
- **cadlab's texts were not where KiCad's are.** A board text was a string, a center, a height and an angle;
  KiCad's has a justification, a character width, a stroke, lines, a slant, mirroring and a knockout, and the
  import dropped them all, so a left-justified pin label was drawn centered on its anchor, across the connector
  it stands next to. Board texts now carry a `style` (BOARD.md, "Board texts") and one function
  (`board::text_ink`) draws them for the DRC, Gerber, IPC-2581, the renders and copper. Hidden texts are no
  longer imported as visible.
- **KiCad justifies a box, not the strokes.** With the style modeled, texts were still 0.05 to 0.15 mm off.
  Probe texts plotted by KiCad (294 of them in the end) gave its conventions: the box around a text reaches
  0.1224 *h* + 0.052 *t* under the baseline, *h*/21 − 0.052 *t* over the capitals and 0.658 *t* beyond each
  end (height *h*, stroke *t*); lines are 1.61 *h* apart; a stroke is at most a quarter of the character size; a
  footprint text that would read upside down is turned unless `unlocked`; a knockout box is *h*/9 beyond the
  strokes. The values were fitted to the coordinates in KiCad's Gerber files, which they reproduce to the
  nanometer.
  cadlab keeps its own anchor (on the strokes: a text that starts at its anchor starts there) and converts at
  the KiCad boundary, both ways and exactly, instead of taking KiCad's box into its model; line pitch, slant and
  knockout margin, which are free choices, take KiCad's values so that imported texts keep their place. After
  that every probe agrees within nanometers vertically and one font unit horizontally.
- **The fonts are two, and that is the residual.** The probes also measured the glyphs: KiCad's stroke font
  has the Hershey simplex advance widths cadlab's has (the alphabet is 24.44 mm wide in KiCad's, 24.39 mm in
  cadlab's, at 1 mm), but most of its letters end one unit (1/21 of the character width, 0.048 mm at 1 mm) inside
  cadlab's on each side, and a few glyphs differ more (`1`, `l`, `i`, `I`, punctuation). cadlab does not copy
  KiCad's glyphs (D56), so a text that KiCad draws 0.01 mm clear of a line may touch it in cadlab's font and the
  reverse. These are recorded per board (below), not allowed by rule.
- **`drc.silk_overlap` checks what KiCad checks,** established by 112 rule probes at 0 and 0.15 mm: a text
  against every other silkscreen item, its own footprint's drawings included (D82 left a designator on its own
  footprint's drawing alone; KiCad reports it and it reads as badly), and drawings of different owners against
  each other (two footprints; a footprint and a board drawing). Not a footprint's drawings among themselves, not
  two board drawings, nothing across sides, nothing hidden: as KiCad.
- **`drc.silk_edge` no longer reports what is wholly off the board,** except a designator. KiCad measures
  silkscreen to the edge from either side and says nothing about silkscreen beyond it and clear of it (the
  outline of an overhanging connector, notes next to the board): nothing there is cut, so nothing is wrong.
  A designator that is not printed at all stays a finding (cadlab places designators; KiCad has no such rule).
- **Footprint texts on the silkscreen become drawings of the footprint** (their strokes, in cadlab's font, where
  KiCad prints them) instead of being dropped: the legend, the DRC, the renders and the KiCad export then have
  them without a text object in the footprint model, whose library footprints are shared by placements while a
  KiCad footprint text has its angle on the board. A footprint that differs only by such a text gets its own
  library footprint, as it did for any other drawing. The price: KiCad checks a footprint text against its own
  footprint's drawings (any pair with a text), cadlab takes a footprint's drawings as one item.
- **The designator keeps its stroke** (`ref_text.thickness`, `silk.set --stroke`): KiCad draws each designator
  with the stroke its field gives (0.1 mm on corne-cherry's 0.5 mm texts), and cadlab printed them all 15 % of
  their height or `min_silk_width` thick, half as thick again there.
- **Silkscreen drawings of footprints that become board holes** (mouse bites) are kept as board graphics.
- **Real differences, left as they are and recorded:**
  - KiCad takes a circle at its radius, without its stroke: a 0.12 mm circle is 0.06 mm smaller for its
    silkscreen checks than it prints (probes: a circle overlapping a line by 0.05 mm is not reported, one
    0.1 mm away passes a 0.15 mm rule). cadlab measures the ink.
  - KiCad takes drawings on the Margin layer as board edges; cadlab's edge is the outline the fab cuts.
  - Tolerance: cadlab's 2 µm (D56) against KiCad's, under 2 µm here (an overlap of 2 µm and a gap 2 µm short
    of the rule are KiCad's alone), on overlaps and distances alike.
  - KiCad names one silkscreen segment per finding and, for silkscreen on a mask opening, often no pad;
    cadlab one footprint and one pad. The comparison folds both ways (`PER_SEGMENT`, `PER_PAD`).
- **The comparison measures what it allows.** A manifest entry for a font difference carries `text_within`:
  it covers only findings that name a text, and a KiCad finding only when cadlab measures the two items no
  farther apart than that many millimeters. An unmatched silkscreen finding is logged with cadlab's distance.
  `CADLAB_CORPUS_REPLAY=<dir>` replays KiCad's DRC reports of an earlier run (CI's `corpus-report` artifact)
  against the current code without KiCad, which is how the causes above were separated between CI runs.

**Measured** (CI, KiCad 10.0.8, 2026-11-13; the corpus job's reports are its `corpus-report` artifact):
- The corpus comparison has no unexplained difference again, with both silkscreen rules compared on every board
  and both comparisons (original, re-export). Both tools report, and the comparison pairs, for example 77 edge
  findings and 2 overlaps on glasgow-revD1, 30 overlaps on glasgow-revC3, 86 overlaps and 83 silkscreen-on-pad
  findings on tinytapeout-demo's re-export.
- What is left, per board, in the manifest (30 DRC entries for 261 findings at most, 14 Gerber layers; before
  D82: 23 for 209, and 18):
  - fonts (`text_within = 0.05`): nrfmicro's re-export, 4 designator overlaps cadlab alone reports;
    buspirate5-rev10, 1 label KiCad alone reports (0.02 mm clear in cadlab's font); glasgow-revC3, 11 overlaps
    and 1 edge crossing cadlab alone reports (two-line labels in brackets, an underscore, two slashes);
  - circles without their stroke: glasgow-revC3 TP3, glasgow-revD1 TP21 (cadlab alone);
  - Margin layer: nrfmicro, 14 KiCad findings on the original and 27 on the re-export;
  - sweep-v2.2: 2 KiCad findings on back-silkscreen arcs of front hole footprints (4 before: the front arcs are
    now board graphics and reported by both).
  Gone from the manifest: cynthion's copper-to-edge entry (6 findings) and four of its six copper layers' Gerber
  differences (the layer numbers are where KiCad draws them: 0.05 to 0.12 mm² per layer became under 0.006 mm²
  on four layers, and 0.02 mm² on the two whose digits, `1` and `4`, are drawn differently).
- The re-exports read in KiCad as the originals do. Its own silkscreen findings on cadlab's re-export, before
  and after: glasgow-revD1 199 `silk_overlap` (its report's cap), 199 `silk_over_copper` and 98 edge findings,
  now 2, 5 and 77, the original's; cynthion 199, 199 and 9, now none, as on the original; buspirate5-rev10 199,
  199 and 28, now 7, 69 and 20.
- Probes: the 93 convention probes agree with KiCad's plots within 3 nm (1 µm is asserted), knockout boxes within
  4 µm (KiCad builds them around a polygon of the strokes); 87 of the 112 rule cells get the same answer from both
  DRCs at both clearances, 25 differ for the reasons above, each asserted.
- Cost: cadlab's DRC takes as long as before on every corpus board, within a few percent (glasgow-revD1, the
  longest: 41.7 s before and 43.1 s after on CI's runners, 37.5 s and 36.1 s on one machine; the silkscreen
  rules use the grid index over bounding boxes they had), and the benchmark job's baselines hold.
- Wrong guesses the manifest caught: four designator pairs on corne-cherry, a label at tinytapeout-demo's edge
  and a designator on nrfmicro were first recorded as font differences. They were gone one run later, which
  had given designators their own stroke, replaced the first estimate of KiCad's box by the fitted one and
  stopped counting a text that touches the edge within the tolerance as crossing it (which change removed
  which was not separated); the entries, then unused, failed that run and were removed.

**Not done, not verified:**
- 23 of cadlab's 109 `drc.silk_overlap` findings on tinytapeout-demo's re-export cannot be checked: KiCad's
  report stops at 199 findings of the type (113 of them further segments of pairs cadlab reports), and the
  original project ignores the rule. They are counted apart (`beyond_kicad_report_cap`), as before.
- A footprint's text against its own footprint's drawings, and two texts of one footprint, are checked by KiCad
  and not by cadlab (the texts are drawings of the footprint): two probe cells, no corpus finding.
- Texts in outline (TrueType) fonts are drawn in the stroke font at the same anchor (`import.text_font`;
  lumenpnp-mobo and tinytapeout-demo have some): their extent differs and nothing measures by how much.
- A knocked-out footprint text is drawn as plain strokes. `bold` is not modeled: in KiCad's stroke font it
  changes nothing when the file gives a thickness (probed: the same strokes at the same width).
- cadlab draws a board text at least `min_silk_width` thick, KiCad at most a quarter of its height: a text
  under 0.6 mm high with the default rules is exported with a stroke KiCad then thins.
- Board texts are set through the import and the library API only: board graphics have no command yet, so
  neither has a text's style.
- Carried over from D82: designators are not checked against pads; a `silk_to_edge` different from
  `silk_clearance` is not exported to KiCad.

*Why:* a legend is checked for what is printed, so the DRC has to draw texts where every output draws them and
compare every pair of things that can print over each other; and an import has to put a text where its designer
saw it. KiCad's placement conventions are facts about its file format, measurable from outside, and matching
them costs nothing; its glyphs are its own work, so the two fonts stay two and the difference they make is
counted, per board, with a bound.

### D84. Router: the residuals D80 left, diagnosed; grid lines along the finest rows, exits off the grid lines, finer grids after a run that leaves a few connections (2026-11-13)
D80 left three groups of failures the router could be blamed for. Each was reproduced at the 200 M work budget (seeds
1–3, D80's numbers to the connection), looked at in place (`route_bench`'s new `CADLAB_BENCH_DUMP` and
`CADLAB_BENCH_FANOUT=only`, the original board's copper read as geometry from its `.kicad_pcb`), and named.
**nrfmicro's D+/D− crossing (2–3 of 85 open).** Not the crossover: D+ and D− routed alone join their pads, and the
failures appear as CC1, CC2, VBUS and the diode's net are added. D− has to loop around the D+ crossover via, and a
diode pad 1.9 mm behind the receptacle leaves that loop 0.18 mm where 0.2 mm is asked. The original fits its
tracks there within 10 µm of the rules (0.608 and 0.605 mm from via centers where 0.6 mm is asked), through lanes
0.15 mm wide that the 0.2 mm grid does not hit, and negotiation never settles. *Decision:* a whole run that leaves a
few connections unrouted (at most 4, or a tenth) without ending on its budget is made again at 1/1.5, 1/2 and 1/3 of
its pitch, then the same with the next seed, until one routes everything; the best is kept. Only when other nets'
routing was in the way of a connection it left (a wall or a pad no track leaves fails on any grid), or with the
grid search alone. A finer run needs four times the first run's cost left in the budget (then twice the
costliest so far), and an explicit `grid` is used as given. nrfmicro: 0 open for seeds 1 to 12.
**Fine-pitch pads with no exit (9).** *tinytapeout-demo's QFN-80 (7):* the grid origin followed the majority of
all pad centers, which left the QFN's rows 0.0925 mm off the grid lines. Every pad then turns onto a line inside
the row, and no assignment serves all of them: two neighbors turning the same way at the same depth are 0.28 mm
apart on their diagonals (0.385 mm needed for two 0.185 mm tracks at 0.2 mm), two turning towards each other end
on adjacent lines. The original leaves every pad straight for 0.8 to 1.1 mm and fans out at 45° beyond, which is
the grid's own behavior once the rows are on its lines. *Decision:* per axis, the origin puts the most pads of
the finest rows along that axis on lines (rows that would need escapes off them, pad pitch a multiple of the
grid's), then the most pad centers. The QFN needs 110 escapes instead of 150 and loses none. *corne-cherry's CC1
(1):* between a no-connect pad and the D− exit turning towards it, it has to turn away by 0.25 mm before that exit
does, and no grid line lies there. *Decision:* a pad with no candidate may turn by a multiple of 50 µm, run on,
and take a stub onto a cell further out. *lumenpnp-mobo's U5.10 (1):* not fixed, see below.
**Pads still cut off from a pour (6).** Diagnosed, not fixed (below).
**Result** (seeds 1/2/3, zero DRC errors other than unrouted before and after, identical on 1 and 4 threads on the
boards checked): open connections 254/251/297 → 227/236/222, 802 → 685 over the seeds. nrfmicro 2/2/3 → 0/0/0,
tinytapeout-demo 29/23/28 → 25/10/13, corne-cherry 4/5/5 → 2/5/2, glasgow-revD1 137/145/170 → 122/131/124,
buspirate5-rev10 4/0/7 → 2/4/1; glasgow-revC3 35/32/34 → 35/35/34 and cynthion 32/27/33 → 30/34/31, each 3 worse
over the seeds; lumenpnp-mobo, sweep-v2.2 and the three small boards unchanged. The four boards in the middle of
that list (revD1, revC3, cynthion, buspirate5) changed only because their grid origin moved by 0.03 to 0.1 mm: that
is another negotiation, and their differences are its spread, not gains or losses of a mechanism, except one
loss that is real: a net-tie pad of revD1 (NT1.2) had grid access at the old origin and has none at the new one
(2 connections in every seed). Generated benchmark: every board routes exactly as on the commit before, so the
goldens are not blessed (stm32-4l, qfp-qfn-4l, fine-4l and bga144-4l sit within tolerance of them as they did).
**Exit criteria.** *M5* is not marked done. nrfmicro now meets it, with buspirate-flash-sop, buspirate-rs232 and
lumenpnp-ringlight; sweep-v2.2's 3 are the design's. But the line says 2- and 4-layer boards of at most 200 nets,
and counting nets (D80 named only the five small boards) three more corpus boards qualify and do not route to
100 %: corne-cherry (2 layers, 152 nets of two pads or more, 2/5/2 open), buspirate5-rev10 (4 layers, 183 nets, 2/4/1) and
tinytapeout-demo (4 layers, 137 nets, 25/10/13). lumenpnp-mobo has 206 such nets and glasgow-revC3 226,
just over the line. Whether "typical hobby board" includes a 0.4 mm QFN-80 or a
440-connection keyboard is a judgment the line does not make, so the gap is stated instead. *M6* unchanged.
**Tried and dropped.** A crossover for the case where the loop has no room (both pads of one net get vias, both
exits of the other go on past them and the pad behind): alone it moved nrfmicro's failures to three other nets
(4/3/2 failed), and with the finer grids it routed 11 of 12 seeds where they alone route 12. Escape assignment
tried again with every pad's candidates ordered to turn the same way, keeping the better: as many exits and the
same pads missed on all eight boards with fanout. More negotiation iterations on nrfmicro, with that crossover
(120 with stagnation at 30: 2/3/1 failed; 300 at 60: 3/0/1). The grid origin following every fine-pitch row,
also those not commensurate with the pitch: stm32-2l went from 58 to 72 vias.
**Known residuals.** *Pours:* a pour round plans the cut-off net again whole, and its new routing cuts other
places. On tinytapeout-demo, seed 1, four rounds each planned two nets again (what is open is GND and +3V3) and
the last was undone, leaving 25 open with nothing failed in the plan; on lumenpnp-mobo the single round ends with
negotiation's share of the budget; on buspirate5-rev10, seed 2, the third check found more open than the second.
The likely fix is to add connections for the cut-off pieces and keep the net's routing (incremental, where D80's
follow-up run was whole); not built, so not shown. *U5.10 of lumenpnp-mobo:* a GND pad the pour joined when the
fanout was planned, so no exit was reserved for it; 0.06 mm off its grid line (a track along the line passes
0.165 mm from the next pad, 0.2 mm asked), it has no grid access once its neighbors' tracks cut the pour. Why the
exact gridless search does not reach it either was not established; the retry that moves other nets starts at
186 M of the 200 M. *NT1.2 of glasgow-revD1*, above. *Cost of the finer grids:* a board that congestion leaves a
few connections short now spends the rest of its budget on runs that may change nothing (the QFN-40 of
`tests/route.rs`: 39 of 40 either way, 44 s of the 60 s budget in a release build instead of a few seconds; nrfmicro
seed 1: 99 M units instead of 8 M, for 85 of 85). The default budget is a time, so this is bounded by what the
user allowed, but an agent that routes, looks and moves parts waits longer for each incomplete answer; whether
that trade is right outside the corpus is a judgment this pass does not settle. *Work count:* on glasgow-revC3,
seed 1, `stats.work` differs by 1108 units of 201 M between 1 or 2 threads and 4 or 8 (tracks, vias, length and
every iteration's overuse identical); the version before does not show it on revC3 (seeds 1 to 3) or cynthion
(seed 1). Nothing in this change touches the meter or the batches, so it is taken for a latent property of a
budget that ends inside a parallel batch, met by this run; not diagnosed. The rest is congestion at the budget
on the seven large boards.
*Why:* the three groups were not one problem and none was what it had been taken for: nrfmicro's was a grid too
coarse for a hand-fitted corner, not a missing crossover; the QFN's was the grid's origin, not the assignment;
the pours' is the granularity of the re-plan. A fix was kept only where the corpus and the generated benchmark
both showed it, and the mechanism built for the first diagnosis was removed when the measurement said the
simpler one did the work.

### D85. guide as a second router backend: an adapter in the library, `router: guide` on `route.all` and `route.nets`, the default unchanged (2026-11-14)
[guide](https://crates.io/crates/guide) (MIT, same owner, its own repository) is a placement and routing engine whose
roadmap expects cadlab to adopt it. This pass makes cadlab able to route with it and measures both routers on the
same boards, so that the adoption can be decided from numbers. It decides nothing about the native router.
**Dependency.** `guide = { version = "0.0.2", default-features = false, features = ["pcb"] }`, from crates.io. Its one
dependency is polyclip 0.0.6, the version cadlab uses, so the tree holds one polyclip and one new crate; `deny.toml`
needs nothing (MIT, crates.io, no second version of anything; `cargo-deny` is not installed here, so this is read
off the file and `Cargo.lock`, not run). MSRV 1.95 for both, checked with `cargo +1.95 check --all-features`.
guide is 0.0.x: cargo treats each release as incompatible, and guide pins polyclip the same way, so the two move
together or the tree gets two polyclips (D16: a standalone crate in its own repository, like polyclip, not a
sub-crate of cadlab).
**Adapter** (`router::guide`, an algorithm module: it uses `board`, `drc`, `lengths` and the model, nothing of the
command layer). The board goes to `guide::pcb::Design` through guide's API and tracks and vias come back; no DSN
file in between, which could carry neither zones nor edge clearance nor hole rules (ROUTER.md, "Specctra"). What
maps to what is the table in ROUTER.md, "guide as a backend". Pads go over with their exact shapes where
`board::exact` has one: guide's shape model is the same (a point, segment or polygon grown by a radius).
**The same guarantees as the native router.** (1) *Verified by cadlab's DRC*: `drc::check_copper` on the board with
the new copper; a connected piece holding a flagged item is taken out, and nothing flagged is applied. guide's own
check is not consulted. (2) *The count is the board's* (D80): completion is read from the board with its pours
filled again by cadlab. (3) *No via in a pad*: the native router's rule, which is not a DRC rule. guide places vias
in pads on its own (40 of 148 vias on soic24-2l, 52 of 106 on fine-4l, 353 of 613 on cynthion) and has no switch,
so the adapter makes every pad a keep-out for vias. A via in an SMD pad needs filling and capping, which the fab
profiles record as an option several fabs do not offer; a result that uses them is not the same result.
(4) *Zone filling stays cadlab's*: guide's fills are its plan and are dropped. They differ from cadlab's in places
(a pad guide's fill joins and cadlab's does not: one each on buspirate-flash-sop and buspirate-rs232), so the nets
with zones that the refilled board leaves open go to guide again on cadlab's fills as fixed copper (*pour rounds*,
at most three, kept when fewer connections are open).
**Where it is offered.** `router: guide` beside `grid | gridless | auto` on `route.all` and `route.nets`: a choice
on the existing commands, not a command of its own. `route.connection` refuses it (`route.guide_unsupported`: guide
routes whole nets and has no push-and-shove), and so do `layers` and `work_budget`. Options guide has nothing for
(`seed`, `effort`, `fanout`, `neck`, `any_angle`, `gridless`, `arcs`, `arc_radius`, `shove`) are named in one
warning rather than refused, so a script written for the native router still runs. The other `route.*` commands take
no `router` and stay native.
**What guide cannot be told**, passed the stricter way and reported (D76's rule for formats without an exception):
keep-out net exceptions (`route.guide_keepout_exception`: the keep-out holds for every net), custom rules by area,
courtyard or kind (`route.guide_custom_rules`: copper that breaks one is taken out by the verification), net ties
(`route.guide_net_tie`). And what it cannot do: stop. guide's PCB router has no time limit, no work count to stop
at, no cancellation and no progress callback, so the time budget of the commands cannot be kept and a run is one
blocking call; a run over its budget says so (`route.guide_no_budget`). Running it on a thread and abandoning that
thread at the budget was considered and not done: the thread would go on using a core for as long as guide needs,
and there would still be no partial result to return.
**Measured** (ROUTER.md, "Native router and guide, D85": the seven generated boards and the twelve corpus boards;
the native router with seeds 1 to 3, the corpus at the 200 M work budget, reproducing D84 to the connection; guide
once per board, as it has no seed, each run killed at 1200 s, which is 7 to 20 times the native run on the large
boards). *Generated boards:* the native router 7 of 7 at 100 % on every seed; guide 6 of 7 (bga144-4l 90.1 %: one
net of 15 pads unrouted as a whole), 2 to 70 times the time (stm32-2l 82 s against 1.2 s, fine-4l 138 s against
2 s), 7 to 27 % more track on six boards. *Corpus under cadlab's rules:* at the limit guide has a result on seven
boards and leaves 139 connections open on them where the native router leaves 13 to 22 (nrfmicro 15 against 0,
corne-cherry 95 against 2 to 5, lumenpnp-mobo 21 against 8 to 14, sweep-v2.2 8 against 3; the three small boards
the native router completes are complete). Given longer: buspirate5-rev10 26 open after 39 minutes (native 1 to 4
in a minute), cynthion 28 after 26 minutes (native 30 to 34 in a minute and a half). No result on
tinytapeout-demo and glasgow-revC3; glasgow-revD1 was killed with vias in pads allowed and not run again.
*guide as it is, vias in pads allowed:* cynthion 8 open in 6 minutes, the 8 pads that are infeasible under the
board's rules (D72), with 353 of 613 vias in SMD pads; elsewhere within a few connections of the rows above.
*Both:* zero DRC errors other than unrouted on every result; nothing of guide's was taken out by the verification
once the outline bug below was fixed; guide's results repeat exactly (1 and 4 threads around it, three builds).
The native router on one thread took 35, 36 and 50 s on buspirate5-rev10, lumenpnp-mobo and glasgow-revC3, so its
threads are not the difference.
**Found on the way and fixed.** (1) `board::exact::item_core` found a pad by its number and position, and two pads
of a footprint can share both (an exposed pad and the thermal via pad at its center: buspirate5-rev10's RP2040,
glasgow-revC3's U1, two parts of lumenpnp-mobo). The exposed pad got the via pad's outline: guide was told a
3.2 mm pad was a 0.6 mm circle and routed two nets across it (68 items taken out by the verification, `drc.short`),
and the DRC measured exact distances to the wrong outline. The pad is now the one whose outline has the box of
the item's copper. The DRC's error counts on the twelve corpus boards as imported and the native router's results
on the three boards concerned are the same before and after. (2) A pour round first took every net with zones
that was open, guide's own failures included; on corne-cherry, whose routing is zones used as wide tracks (933 of
them), that was 77 nets and a second run that had not ended after 16 minutes. Rounds are for nets guide took for
routed.
**Default: unchanged.** The native router stays the default and `auto` its search. Under cadlab's rules there is
no board in the measurement on which `router: guide` gives more than the native router: it is level on the three
small boards and on cynthion (28 against 30 to 34, at 16 to 20 times the time), behind on every other board that
ends, and has nothing to show on three. The one clear result in its favor, cynthion with vias in pads, is not
offered by the commands: `RouteOptions::via_in_pad` exists in the library for the measurement, and offering it is
a decision about what a fab is asked to do (fill and cap), to be taken for both routers or neither. So today
`router: guide` is a second opinion for a board the native router leaves short, when the wait does not matter and
nothing has to stop it. The default can be looked at again when guide (1) takes a limit on time or work, can be
cancelled, reports progress and returns the best result so far: without that no command keeps its budget, and
the agent loop of route, look, move a part cannot use it; (2) under the no-via-in-pad rule completes at least
what the native router completes on the corpus in a comparable time; (3) reports what failed per connection,
with a place and a cause, and keeps what it routed of a net it could not finish; (4) takes keep-out net
exceptions and rules by area. The full list of what the adapter met, with a reproduction for each item, is in the
report of this change and not kept in this repository.
*Why:* an adoption decided on guide's own benchmarks would compare its boards, its DRC and its count of a
connection with ours; behind the same commands, the same DRC, the same fill and the same rule about vias the two
routers answer the same question, and the list of what the adapter had to work around is the list of what guide
needs before it can replace anything.


### D86. `sim.sparams --solver ems`: the ems crate as a second full-wave solver on the same region; the built-in solver stays the default (2026-11-14)
`ems` (crates.io, MIT, the same owner's clean-room FDTD solver meant to replace openEMS, on the geometry crate `csx`)
can now run what `sim.sparams` runs: `solver: ems`, default `builtin` unchanged. Table and mapping: ELECTRICAL.md,
"ems as a second solver". Neither crate's source was changed; cadlab reads their public API and documentation
(both MIT, both clean-room themselves), and no openEMS, CSXCAD or other GPL code was read.
**One model, three consumers.** `openems::xml::write` built the export's XML directly from the region. It is now
two steps: `xml::properties` turns the region into a list of properties with primitives (materials, Debye
materials, metal, conducting sheets, lumped elements; boxes, prisms, cylinders, with the priorities), and the
writer prints that list (the golden file is unchanged byte for byte). `fdtd::ems` turns the same list into a
`csx::ContinuousStructure`. So "ems runs the model the export writes" is true by construction, and a change to
the export's geometry reaches both. The region's preparation was split the same way: `fdtd::extract` (region,
mesh refinement, cell guard, part candidates), `fdtd::place_ports` (ports and probes as mesh nodes) and
`fdtd::Waves` (waves to S, the run warnings, the result) serve both solvers; only the voxelization and the time
stepping are the built-in solver's own.
**Not through csx's XML writer.** Writing `export.openems`'s file with `csx`'s XML writer was considered and
left: its output is not the file the export writes (it adds colors, weights and isotropy attributes, prints
floats rather than the exact decimal of the integer nanometers, and has no place for the comments that carry the
stackup's εr and tan δ, the fit's band and the roughness factor), so the golden file and the documented format
would change, and whether openEMS's reader takes the result the same way can only be shown by the oracle job,
which does not run here. The shared property list gives the equivalence that mattered without touching the file.
**Ports.** Lumped ports are ems's `LumpedPort`. Microstrip ports are not ems's `MslPort`: that helper needs a
zero-thickness strip and three equally spaced mesh lines, and reports at its own extracted line impedance; cadlab
writes the feed as a lumped port and the measurement as plain csx probes on the planes the built-in solver
uses, so both solvers measure the same quantity at the same plane with the same reference impedance.
**A feature, on by default.** `ems` and `csx` have no dependencies of their own and add 21 s to a release
build, so the feature is in the default set, like `models3d`; without it the option answers
`sim.solver_unavailable`. MSRV 1.95 and the license list hold (both MIT, from crates.io). Dev and test builds compile the
two crates at `opt-level = 2` (`[profile.dev.package]`): the test that runs ems takes 37 s in a debug build
instead of 218 s.
**`threads`.** New on the command for both solvers (the box this was measured on is shared); results are
bit-identical for one, three and four threads in both.
**What was measured** (the table has the numbers; ems 0.0.6, csx 0.0.2). Lossless lines, a stripline, Debye
laminates, lumped parts and the cavities agree to three or four digits, and ems is two to five times faster
there. The plate cavity's Q from ems's conducting sheets is within 0.2 % of h/δ. Against that: (1) lossy `solid`
copper, cadlab's default, has to be two sheets a copper thickness apart, and reads a conductor loss uneven over
the band, 0.089 dB against 0.044 dB at 6 GHz on the oracle's line, with 43 % more timesteps to decay; (2) diagonal
copper on sheet or lossy copper is a staircase (143.5° against 157.5° for arg S₂₁ at 10 GHz on D73's line, the
built-in solver's own staircase value); (3) roughness is exact at f₀ only; (4) a ground plane of sheet edges
makes ems two to six times slower than the built-in surface impedance, with 80 to 200 MB where that takes 10.
On session 6's matching network (lossy sheet copper with diagonal tracks, both limits at once) the two differ by
0.04 in |S11| and 0.018 in |S21| at 2.44 GHz, ems taking 271 to 338 s for the built-in solver's 83 s.
**Decision.** The default does not change and `src/fdtd/engine.rs` stays. ems is the cross-check that runs
everywhere (openEMS is CI only), and the faster solver for regions without lossy copper. Switch the default when
ems has a surface impedance on the faces of solid metal or conformal conducting sheets (1, 2), conformal
outlines for sheets on mesh planes (2), a roughness factor over the band (3) and a sheet that costs about what a
surface impedance does (4), and the table above then holds within the built-in solver's test tolerances; delete
the built-in solver one release after that, once the oracle job has compared ems with openEMS on the three
oracle cases. Until then two solvers cost little: they share everything but the time stepping.
*Not established:* why the two-sheet copper rings in ems and not in openEMS (the same model there agrees with
the built-in solver within a few percent); the effect of ems's uncorrected sheet edges inside the absorbing
layer, which it warns of on every lossy run (the lossy lines agree within 0.03 dB with them); memory was measured
as the process's peak, compile step included, not per array.

### D87. ems 0.0.7 in `sim.sparams --solver ems`: its lossy copper, conformal outlines, typed warnings and one compile; what running them together found; the built-in solver stays the default (2026-11-14)
D86 listed what ems needed before it could replace the built-in solver. ems 0.0.7 has all four: a surface impedance
on the faces of solid metal, conformal outlines for sheets on mesh planes, a roughness factor over the band, a
sheet model at the cost of a surface impedance; and three things for a program that drives it: typed warnings, one
compile for several runs, a port voltage as the mean over the port's columns. `fdtd::ems` uses them; the table
and the mapping are in ELECTRICAL.md, "ems as a second solver". No openEMS, CSXCAD or other GPL code was read;
the changes to ems below were made from its own documentation and code (MIT, clean-room, same owner).
**Adopted.** Lossy `solid` copper is the net's `Metal` volumes with `with_lossy_metal` (a surface impedance on
every face), not two conducting sheets a copper thickness apart: `openems::xml::Geometry::volumes` keeps the
volumes for ems and the export writes what it wrote (golden file unchanged), so "ems runs the model the export
writes" now holds except for lossy solid copper. Sheets are `SheetModel::SurfaceImpedance`. Roughness is ems's
Hammerstad–Jensen or Huray factor on both (`sim.ems_roughness_at_f0` and `sim.ems_copper_faces` are gone). The
model is compiled once (`CompiledModel`), each port's run built from it. Lumped ports read the mean voltage of
their columns, which is the built-in solver's definition: on 2 mm and 1 mm strips with perfect sheet copper the
two solvers' |S₁₁| were 0.036, 0.025 and 0.0044 apart with ems's centre column and are 0.016, 0.003 and 0.002
apart with the mean. ems's warnings arrive typed and become `sim.ems_<code>` with the net, layer, component or
port behind the property as subjects; the substring filter is gone, what cadlab makes on purpose is dropped by
code.
**The combination that had not been run** (asked for with this work: ems's lossy conductors and its conformal
outlines were written separately). D73's bent line, 0.25 mm cells, four copper models, against the built-in
solver (arg S₂₁ at 10 GHz 157.0° sheet, 157.1° solid; ems's staircase 143.0°). Every run converged with Σ|S|²
at most 1.004; the instabilities below showed in a closed box, not here:

| Copper in ems | As handed over (ems master 4dd8023) | Now (ems branch `cadlab-adopt`, to be 0.0.7) |
|---|---|---|
| Perfect sheet | 153.7°, stable | 157.1° with cadlab's threshold for small faces (below) |
| Lossy sheet, ladder | 153.4°, stable, converged, Σ\|S\|² ≤ 0.9998; conductor part of \|S₂₁\| at 3 / 5 / 7 GHz 0.029 / 0.039 / 0.047 dB (built-in 0.039 / 0.049 / 0.059); 159 s | not used by cadlab |
| Lossy sheet, surface impedance | panic at compile: the model indexed past its roughness list on the first cut edge csx had not selected, and took no cuts | 156.6° (built-in 156.5°); 0.037 / 0.048 / 0.058 dB; 63 s (built-in 31 s) |
| Lossy solid | 156.7° (built-in 156.7°), converged; conductor part 0.039 / 0.052 / 0.063 dB against 0.034 / 0.043 / 0.052 (17 to 21 % high); in a closed box a copper strip at 45° grew without bound within 100 periods | 156.1°; 0.037 / 0.048 / 0.058 dB (9 to 11 % high); 59 s (built-in 65 s); no growth |

Fixed in ems, each with a test there (`tests/conformal_outline.rs`, a half-wave strip resonator in a closed box,
where nothing hides a growing mode): (1) the rational sheet takes the cuts of its outline as the ladder does
(covered part in series with the open part, loading around cut faces, the stored voltage): the ladder's resonance
to 0.04 %, Q 33.8 / 36.8 / 35.2 along x, at 45° and at 17°; (2) a surface edge of solid metal keeps at least the
capacitance of its open quarters: its capacitance was sampled on the drawn metal, which reaches past the
staircase, so some edges had none and were resistors integrated explicitly (a strip of 2e5 S/m grew within 20
periods without any conformal cell; copper with conformal cells within 100); (3) the edges a solid outline cuts
count, by their covered fraction, for the surface width of the whole edges beside them: a copper strip's Q was
639 along x and 392 at 45°, now 602; (4) `metal.lossy_thin` and `metal.lossy_cut` as codes of their own; (5)
`CompileOptions::conformal_solids`: with `Slanted`, solid metal along the mesh keeps the staircase, as sheets
already did. cadlab's mesh puts a line a third of a cell inside a strip's edge for the staircase; with every
surface cut the oracle's line read Z₀ 49.07 Ω against the built-in solver's 49.85 Ω and |S₁₁| 0.017 apart, with
`Slanted` 49.88 Ω and 0.005. What stays an approximation in ems: the covered part of an edge cut by *solid*
lossy copper is a perfect conductor (`metal.lossy_cut`, dropped by cadlab).
**Three choices on cadlab's side.** (a) ems takes the current of a cut face under half its area from the faces
around it; safe on any mesh, and 13° short of the built-in solver on the bent line at two cells per width
(141.0° against 154.3°). cadlab lowers that threshold and the deviation currents' floor to 0.05, 0.1 or 0.2
while ems's stability estimate stays under 0.9, else keeps 0.5: 153.1° on that line (estimate 0.64; 0.85 on the
via board), and ems's default on cubic cells, where ems's own strip tests estimate 1.19 at 0.1. The estimate is a
power iteration from below, hence the margin; a threshold chosen in ems from the timestep and the cells, as the
built-in solver's bound is, would be the proper place. (b) A diagonal track under 1.5 cells wide is in pieces
with ems's cut edges (session 6's matching network: |S₂₁| 0.574 where the built-in solver reads 0.798, and 0.791
as a staircase). (c) A plated hole under two cells across: on the four-layer via transition (0.3 mm drill, 0.6 mm
pads, 0.3 mm cells) ems's conformal cells read |S₂₁| −0.598 dB at 4 GHz where the built-in solver reads −0.408 dB
and both read −0.452 and −0.456 dB as staircases (openEMS in CI, a staircase: −0.457 dB); with perfect sheet copper
−0.579 dB against −0.375 dB, staircases −0.426 and −0.440 dB, and the built-in solver's staircase on 0.2 mm cells
−0.348 dB. ems's conformal cells are further from the finer mesh than the staircase, the built-in solver's
nearer. In cases (b) and (c) the region runs as a staircase in ems with `sim.ems_staircase`, which no longer
means what it meant in D86.
**The bar of D86**, case by case (ELECTRICAL.md has the table):
- *Lossy solid copper* (the oracle's line, solid copper, lumped ports): met. |S₂₁| within 0.006 dB and |S₁₁|
  within 0.0046 over 0.5 to 6 GHz; the conductor's part 0.0176 / 0.0301 / 0.0436 dB against 0.0176 / 0.0300 /
  0.0438 dB at 1.05 / 3.03 / 6 GHz (ems 0.0.6: 0.015 / 0.040 / 0.089); the steps of perfect copper; 20 s against
  29 s (306 s).
- *Diagonal copper* (D73's line): met. Perfect sheets 157.6° against 157.5° at four cells across the track and
  153.6° against 154.8° at two (ems 0.0.6: 143.5° and 121.6°); lossy sheets and lossy solid copper within 0.6° at
  four cells, 1.1° and 3.6° at two, with the conductor's part of |S₂₁| within 6 % for sheets and 9 to 11 % high
  for solid copper at four cells (3 and 7 % at two).
- *Roughness over the band:* met. Hammerstad–Jensen, 1 µm: the conductor's part 0.0212 / 0.0449 / 0.0730 dB
  against 0.0213 / 0.0447 / 0.0725 dB.
- *A sheet at the cost of a surface impedance:* met in time (the lossy sheet line 11.6 s against 12.2 s; 71 s
  before), not in memory (90 MB against 9 MB).
- The rest of D86's table is unchanged to the digits given there (lossless lines, stripline, laminate, parts,
  cavities), and session 6's network is within 0.008 in |S₁₁| and |S₂₁| (0.04 and 0.018).
- *Not at parity: vias.* With the staircase rule the transition is within 0.048 dB in |S₂₁| and 0.014 in |S₁₁|
  of the built-in solver (0.095 dB in D86, with ems 0.0.6 a staircase by nature), inside the 0.25 dB the oracle
  test allows between openEMS and the built-in solver; but that is ems without its conformal cells, where the
  built-in solver has them and is the one nearer the finer mesh.
**Decision.** The default does not change, and `src/fdtd/engine.rs` stays. ems 0.0.7 clears the bar D86 set, on
lossy solid copper and on diagonal copper, within the tolerances of the built-in solver's own tests (0.01 in
|S₂₁| and 0.015 in |S₁₁| in `ems_runs_the_region`; |S₂₁| within 15 % + 0.025 dB in the oracle test). It does not
become the default with this change for four reasons, in order: (1) with a via in the region ems now runs a
staircase where the built-in solver has conformal cells, which is the built-in solver ahead on the commonest RF
structure of a board; (2) three of cadlab's choices above work around ems (the threshold for small faces, the
two staircase rules) and belong in ems, where they can be tested on more than cadlab's cases; (3) the oracle job
has not compared ems with openEMS on the three oracle cases, D86's condition for deleting the built-in solver;
(4) memory. Switch when ems's conformal cells hold on a via (or ems itself keeps a staircase where they do not)
and its small-face threshold is its own, then run the oracle job on ems.
*Not established:* why ems's conformal cells move the via transition away from the finer mesh (its sheet
outlines and its barrel each contribute, 0.13 and 0.04 dB at 4 GHz with lossy sheet copper; pads of one cell in
radius and a barrel of half a cell are outside what ems validated, holes of ten cells); ems on the 0.2 mm mesh,
conformal or not (2.7 million cells: a quarter of an hour per run, not waited for); why ems's conductor loss on
microstrip ports rises at the end of the band (0.048 against 0.036 dB at 6 GHz on the sheet line, within 2 % at
5.5 GHz, and no such rise with lumped ports); how far ems's stability estimate can be trusted as a bound (it is
a power iteration from below, hence the margin of 0.9); memory was measured as the process's peak, compile step
included.

### D88. guide 0.0.3 adopted: run control, per-connection causes and the host's board model replace the adapter's workarounds; the default unchanged (2026-11-14)
D85 ended on a list of what guide needed before `router: guide` could be more than a second opinion, and what
the adapter had to work around. guide's next release (0.0.3: run control, the connections a run leaves open, and
"what a host's board may say") answers most of it. This pass adopts that API, removes each workaround it covers,
and measures again. `guide = { version = "0.0.3", default-features = false, features = ["pcb"] }`; developed
against guide's master before the release.
**What each workaround became** (`router::guide`; ROUTER.md, "guide as a backend"):
- *No budget, no stop, no progress* (`route.guide_no_budget`, the `work_budget` refusal) → `pcb::route_with` with a
  `RunControl`: `work_budget` is guide's own (search nodes, the same result on any machine), the time budget a
  deadline at four fifths of it, cancellation a flag set from guide's progress callback (four times a second),
  progress guide's phases and counts. A run a budget stops is verified and counted like any other and says so
  (`budget_exhausted`, `route.budget_cut`, the reason of each connection left). Both codes are gone.
- *"guide reports nets, not connections, and no place or cause"* → `Routed::open`: each failed connection of the
  board carries guide's cause as its reason, place, layer, subjects and hints (a pad shut in and by what, no
  way found, a search's node limit, the budget that ended, no common layer). `keep_partial` is on, so a net
  that cannot be routed whole keeps what it could.
- *No seed* → `seed` (other than 0) is guide's, and is no longer in `route.guide_option_ignored`.
- *A via keep-out over every pad* → `vias_in_pads = false`.
- *guide's own fill as its plan, and up to three pour rounds on cadlab's fills as fixed copper* → cadlab's fills
  go over as `Zones::fills` and guide plans on them. One second run is kept, because it still pays: routes cut
  a pad off from its pour on buspirate-flash-sop and lumenpnp-ringlight (one connection each, open without the
  second run and closed with it) and on lumenpnp-mobo. `plane_coverage` stays 0: on corne-cherry 0.1, 0.25 and
  0.5 gave 96 open in 45 s against 91 in 13 s.
- *Keep-out net exceptions passed whole* (`route.guide_keepout_exception`) → `Keepout::except`; the code is gone,
  and guide takes an excepted net through the keep-out as the native router does.
- *`layers` refused* → `RouteOptions::layers`.
- *A pad stack as one pad per shape and a 1 µm via for the barrel* → one `Pad` with `stack`.
- *Net ties as fixed copper of one net* (`route.guide_net_tie`) → `Board::ties`, for the copper between a tie's
  pads only: that is what `board::tied` lets the nets of the group touch. The pads are not tie items, since the
  DRC flags a track of one net on the other net's pad, and guide's tie would allow it. The code is gone.
- *Custom rules not passed* (`route.guide_custom_rules`) → those that are a clearance inside an area or a
  courtyard go over as `RuleSpec::areas`. guide applies an area's clearance between two pieces of copper that
  both touch it, the DRC when either does, so a rule that loosens is routed the stricter way where one piece is
  outside, and one that tightens is left to the verification there. Still passed the stricter way and named by
  the same code: rules by kind, by footprint, by containment, between two scopes, and `disallow`. Never passed:
  a rule's track width and neck-down (guide routes a net at one width).
- *The outline edge by edge* → `pcb::edges`. *Pad overrides by position* → `Zones::set_pad`. A geometry error
  names the board's item (`RouteError::Item`). `fanout_vias` counts guide's escape vias (`Routed::via_kinds`).
**Kept**: cadlab's DRC verifies the result and flagged copper is taken out; completion is counted on the board
refilled by cadlab; no via in a pad; the same result for the same input unless a time budget cut the run (the
budget is part of the input since guide 0.0.4: a run with a budget is by design not the run without one, D91);
`route.connection` is still refused (`route.guide_unsupported`), and the default router is the native one.
**Measured** (ROUTER.md, "Native router and guide 0.0.3, D88"). *Generated boards:* guide completes all seven
(0.0.2: six) in 0.02 to 36 s (0.04 to 138 s), 0.7 to 26 times the native router's time. *Corpus, left to
finish:* guide ends on eleven of twelve boards in 1 to 356 s (0.0.2: seven within 1200 s) and leaves 351
connections open on them, the native router 86 at its default 60 s and 98 to 105 at 200 M work: level on three
small boards, ahead on cynthion (25 against 27 to 34), behind on seven (corne-cherry 91 against 1 to 5,
tinytapeout-demo 116 against 8 to 25, glasgow-revC3 71 against 34 to 35, lumenpnp-mobo 18 against 8 to 14,
nrfmicro 13 against 0, buspirate5-rev10 11 against 1 to 4, sweep-v2.2 6 against 3); glasgow-revD1 is not
done in 16 minutes. *At the default 60 s:* guide is cut on seven boards and leaves 890 open on the eleven where
the native router leaves 86.
**What still blocks it**, most blocking first. *Missing in guide:*
1. *A way out of a fine-pitch pad without a via in it.* 281 of the 351 open connections are pads guide reports
   shut in, 202 by nothing but their own footprint's neighbouring pads (tinytapeout-demo 99 of 116, glasgow-revC3
   57 of 71, corne-cherry 40 of 91). The native router necks the track down or fans out with a smaller via;
   guide has one width and one via for a net. `CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_VERBOSE=1
   CADLAB_BENCH_BUDGET_MS=300000 route_bench --corpus tinytapeout-demo` lists them.
2. *The best result so far when stopped.* A cut run returns the board of that moment, without the nets rip-up
   has out: buspirate5-rev10 160 open cut at 48 s, 200 cut at 240 s, 11 when it ends at 356 s
   (`CADLAB_BENCH_BUDGET_MS=60000`, `300000`, `1200000`). No time budget shorter than the whole run is safe.
3. *Time.* 1 to 12 times the native router's on the boards over a minute (buspirate5-rev10 356 s against 30,
   cynthion 352 against 62, glasgow-revC3 206 against 60), and glasgow-revD1 is cut at 960 s with 311 open.
4. *Stopping on time.* After the deadline guide fills the zones around what it has, which the adapter drops: 8
   to 15 s on cynthion, lumenpnp-mobo and glasgow-revD1, so a 60 s budget takes up to 67 s. A way to return
   without filling would do.
5. *Two escapes on one line.* With `vias_in_pads` off, the escapes of corne-cherry's `right/D+` and `right/D-`
   overlap on B.Cu; guide's own `drc::check` finds the 5 shorts, and the verification takes 45 items out
   (`CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_VERBOSE=1 route_bench --corpus corne-cherry`, `drc_removed`).
6. *Fills as obstacles are slow to measure against:* lumenpnp-mobo's second run takes 66 s for five nets.
*Adapter:* the second run is skipped after a cut run, so pads the routes cut off from a pour stay open there;
the deadline's share of the budget is fixed, not measured; custom rules beyond an area's clearance, `effort`,
`fanout`, `neck`, arcs and `route.connection` have no counterpart.
**Found on the way, not touched:** the native router panics on glasgow-revD1 under the 60 s time budget (seed 1,
3 threads: `engine.rs`, `pour_targets`, a pour index out of range); the work-budget runs do not meet it.
**Default: unchanged.** D85's four conditions: (1) limits, cancellation and progress are there, the best result
so far is not; (2) completion under the no-via-in-pad rule is behind on seven of twelve boards and the time on
all large ones; (3) per-connection causes and partial nets are there; (4) keep-out exceptions and area
clearances are there. So `router: guide` is now a backend that keeps the commands' contracts and says why it
left what it left, and it is still the second opinion: (1) and (2) decide, and both are guide's to do.
*Why:* a workaround in an adapter is a place where two programs disagree about the board; each one removed is
one fewer way for guide's plan and cadlab's check to differ, and the measurement then shows the router, not the
adapter.
**As released:** the tables above were measured on guide's master before its last changes; 0.0.3 as published
adds a stopped run that goes back to the routes that made the most connections, a net too wide to leave its pads
routed narrower, the fix for two escapes sharing a channel, and `RunControl::fill_on_stop`, which the adapter
turns off (cadlab refills). guide's own measurement through this adapter at the 60 s budget: 1023 open over the
twelve corpus boards against 1681 before (buspirate5-rev10 24 against 160, glasgow-revC3 83 against 346,
tinytapeout-demo 56 against 121, corne-cherry 63 against 91, cynthion 63 against 82, lumenpnp-mobo 56 against
71, glasgow-revD1 659 against 791); the generated boards all complete, stm32-2l in 9.3 s. Not rerun here to
completion, so the 300 s and 1200 s columns stand as measured before; the native router's 86 at 60 s on eleven
boards is still far ahead, and the default stays.


### D89. ems 0.0.9 in `sim.sparams --solver ems`: the adapter's workarounds dropped; the default decided against five criteria, and the built-in solver stays the default (2026-11-15)
D87 ended on: switch the default when ems's conformal cells hold on a via and its small-face threshold is its own,
then run the oracle job on ems. ems 0.0.8 had both; 0.0.9, released while this was measured, fixes an instability
0.0.8 had (below), and is what `Cargo.toml` asks for. This change adopts it and decides the default against
criteria set before measuring. The table is in ELECTRICAL.md, "ems as a second solver". No openEMS, CSXCAD or
other GPL code was read; the two small changes to ems below were made from its own code and documentation (MIT,
clean-room, same owner).
**Dropped from the adapter** (`fdtd::ems`), each because ems does it on any model since 0.0.8:
- `SMALL_FACES`, `STABILITY_MARGIN` and the loop of up to three compiles that lowered ems's threshold for small
  cut faces while its stability estimate stayed under 0.9. One `CompiledModel::new` with ems's defaults: a small
  face keeps its own current where the cells around it are stable at the timestep by a cell-wise bound
  (`ConformalOptions::keep_stable`). D73's bent line at two cells across the track: 154.7° (the built-in solver
  154.8°, cadlab's threshold 153.6°, ems's old rule 141.6°).
- The rule for diagonal tracks under 1.5 cells (`unresolved()`), with which such a region ran as a staircase:
  ems keeps whole the edges that carry a narrow slanted conductor (`CompileOptions::conformal_bridges`), the
  edge that continues a lumped element's column into its pad and a sheet's edges under a barrel. Session 6's
  matching network, tracks 1.1 cells wide: |S₁₁| 0.5727 and |S₂₁| 0.7976 at 2.44 GHz with conformal cells, the
  built-in solver 0.5733 and 0.7983 (ems 0.0.7: 0.790 and 0.574, and 0.581 and 0.791 as the staircase).
- `HOLE_CELLS`, the staircase for a plated hole under two cells across: ems integrates the faces a barrel, a pad
  or a corner crosses. The four-layer via transition (0.3 mm drill on 0.3 mm cells, lossy solid copper):
  −0.396 dB at 4 GHz, the built-in solver −0.408 dB, within 0.012 dB at every frequency (ems 0.0.7: −0.598 dB
  with its conformal cells, −0.456 dB as the staircase cadlab ran).
- `sim.ems_staircase`, which those two rules raised. `conformal` means the same in both solvers again.
`ConformalReport::bounded` is not passed on: it is false on ordinary runs that are fine (24 of 556 cut faces
interpolated on the bent line at 0.25 mm), and in the one unstable run found (below) it was false with a
stability estimate of 0.69, as on those. A diagnostic from it would have said nothing that helps.
**Added to the adapter**, so that the built-in solver's tests can be run on ems unchanged: `conformal_edges` in
the result (ems's count of cut edges), a part's `columns` (the mesh columns its box takes in), and properties
without primitives are left out of the model (the mask's openings on a board that has none: ems answered
`sim.ems_other`, "has no primitives"). A run that ends on a negative energy is `sim.unstable`.
**An instability in ems 0.0.8**, found here by `measure_diagonal_copper` and in ems at the same time. The bent
line with lossy `solid` copper on 0.25 mm cells grew: ems ended the run after 7553 of 10 900 steps as converged,
the energy invariant having gone negative, and the result had |S₁₁|² + |S₂₁|² = 1.12 at 10 GHz and |S₁₁| −17.6 dB
at 1 GHz for −43.6 dB. `sim.not_passive` was the only sign. ems's estimate read 0.69; with `keep_stable` off the
run was stable, as was the default mesh. Cause: the cell-wise bound took the edges on the surface of lossy solid
metal for perfect conductors, which they are in the coefficients when the conformal cells are applied; the
surface impedance, installed after, gives them a voltage at their whole length. ems 0.0.9 gives such an edge
the capacitance the faces beside it need (its 6ccf859); a fix of the same cause written here on ems's branch
`cadlab-008` (the bound counting those edges, with a raised capacitance) was dropped for it unmerged. With 0.0.9:
Σ|S|² 0.9985, arg S₂₁ 156.4° (the built-in solver 157.1°). Two small changes remain on `cadlab-008`, neither
needed by cadlab: a negative energy invariant ends a run as `Diverged`, not as decayed; csx's "has no
primitives" has a warning code.
**The band's end.** D87 left open why ems's conductor loss on microstrip ports rose by a third at f_max (0.048
against 0.036 dB at 6 GHz). It was the end criterion: the conductor's part is the difference of two runs, each
ended at −50 dB. To −70 dB the two solvers read 0.0365 and 0.0352 dB there, 3 to 4 % apart as at every other
frequency. The measurements that subtract two runs now end at −70 dB.
**Tests.** The built-in solver's board-level tests take a solver and run on both: `sim_sparams_command`,
`sim_sparams_models_parts`, `cut_cells_do_not_change_a_parts_columns`, `sim_sparams_conformal_option`,
`narrow_diagonal_track_refines_the_mesh`, and behind `#[ignore]` `sim_sparams_microstrip_validation`,
`lossless_line_near_the_pml_is_passive`, `sim_sparams_mask_validation`, `sim_sparams_parts_validation`,
`routed_45_degree_line`. The engine-level tests of `tests/fdtd*.rs` build the built-in solver's own structures;
their ems counterparts are the csx models of `tests/fdtd_ems.rs` (`measure_lumped_fixture`, `measure_cavities`).
`ems_small_plated_holes_are_a_staircase` became `ems_runs_a_via_of_one_cell`. The openEMS oracle
(`tests/openems.rs`) compares openEMS with each solver, to the same tolerance, cadlab's runs first, and a NaN
fails a comparison instead of passing it; it was run here against a stand-in for openEMS only (CI has openEMS).
**The criteria, and what each measured** (release builds, three threads, ems 0.0.9's source; the rows with
perfect copper only were taken on 0.0.8 and not again):
1. *Every validation tolerance of the built-in solver's tests holds with ems.* Not met, by one test.
   `sim_sparams_microstrip_validation` (60 mm of `solid` copper between microstrip ports, 10 GHz, |S₂₁| against
   D52 within 3 % to 2.5 GHz and 6 % above): the built-in solver −2.3 to +4.9 %, ems −1.4 to +7.2 %, over at
   8.02, 8.52 and 9.01 GHz (+6.5, +6.8, +7.2 %); the test holds ems to 8 % and says so. Its Z₀ (within 3 %),
   εeff (1.5 %) and |S₁₁| (0.04) hold, equal to the built-in solver's to 0.1 %. Met: the coarse line with its
   mask, roughness and lumped ports; parts (|S₂₁| within 0.03 and 0.02 of theory, the same columns); passivity
   next to the absorbing layer (Σ|S|² at most 1.0007, the built-in solver 1.0010, of 1.0025 allowed); the solder
   mask (Z₀ 50.62 Ω, εeff 3.1542; the built-in solver 50.61 Ω, 3.1541); the 45° line; session 6 (|S₂₁| 0.066 and
   |S₁₁| 0.094 from the ideal chain, of 0.1 and 0.15 allowed); the cavities and the lumped fixture as csx models.
2. *No case where ems is materially worse in accuracy.* Not shown. The long line above is two points further
   from D52 than the built-in solver from 2 GHz up (0.037 dB of 1.7 dB at 9 GHz), and on slanted `solid` copper
   ems's conductor loss is 10 to 14 % above the built-in solver's (sheets: 3 %). Which is right is not
   established in either case. Everything else agrees closer than before: via 0.012 dB, bent line 0.2° (sheets)
   and 1.6° (`solid`) at two cells across the track, lossy lines 0.006 dB, parts 0.0011.
3. *Run time within 1.5 times on what users hit, lossy solid copper with vias.* Met at end criteria of 40 and
   50 dB: the via transition 82 s against 122 s, the oracle's solid line 18 s against 29 s, session 6 (sheets)
   113 s against 79 s (1.4 times). Not met everywhere at deeper ones (`sim.sparams` ends at −60 dB by default): to
   −70 dB ems takes 1.3 to 1.7 times the built-in solver's steps on the straight lossy lines (still faster in
   time: 88 s against 105 s) and three to four times on the bent line (167 s against 41 s with sheets, 304 s
   against 73 s with `solid` copper); with perfect copper the steps are equal. The via
   transition to −60 dB: 1.4 times the steps, 169 s against 199 s. On one thread a small region with a via takes ems three times the built-in solver's time (102 s against 34 s on
   25 194 cells; 38 s on three threads).
4. *Deterministic for any thread count.* Met: identical Touchstone data on one and four threads (lossless,
   lossy sheets, lossy solid copper), and on one, two and three with conformal cells (the via transition with
   lossy solid copper, the bent line with lossy sheets).
5. *No panics, no instabilities.* Not met by ems 0.0.8 (the run above); met by 0.0.9 on every case of the
   table, `measure_diagonal_copper` now asserting that each copper model ends and stays passive.
**Decision.** The default does not change, and `src/fdtd/engine.rs` stays. Criterion 1 fails by three points of
one test and 2 with it, and 3 holds on the via transition at the default end criterion but not on lossy slanted copper at −70 dB
(−60 dB was not measured there). Switch
when (a) the long line's loss is inside the built-in solver's tolerance, or it is shown that ems is the one
nearer the truth and the tolerance is moved for both, (b) ems's lossy copper reaches −60 and −70 dB in about
the built-in solver's steps, (c) the oracle job has compared ems with openEMS on its three cases, which it does
from this change on. Memory is not a condition. The default was switched in the worktree to run the built-in
solver's tests on ems and switched back; the tests name their solver instead.
*Not established:* which solver is right on the long line above 6 GHz (D52 is a quasi-static 2D model; the
test's 6 % was set from the built-in solver's own +5.0 %) and on the loss of slanted `solid` copper; whether
ems's excess on the long line is the measurement at a microstrip port on `solid` copper or the line (with lumped
ports on 24 mm the two agree to 0.006 dB); what holds the energy of ems's lossy copper at −60 dB; the openEMS
oracle with ems, which has not run anywhere yet; the via transition on 0.2 mm cells with ems (ems's own numbers
are quoted in the table); times on a shared machine vary by a fifth; memory is the process's peak above its
level before the run, which under-reports a run after a larger one.

### D90. ems 0.0.10 in `sim.sparams --solver ems`: the three things D89 measured against it are gone; one case of the run-time criterion is left, and the built-in solver stays the default (2026-11-15)
D89 ended on three things to change before the default could: (a) the long line's loss inside the built-in
solver's tolerance, (b) lossy copper at −60 and −70 dB in about the built-in solver's steps, (c) the oracle job
comparing ems with openEMS. ems 0.0.10 was written against (a) and (b) and against the 10 to 14 % of slanted
`solid` copper. This change adopts it (`ems = "0.0.10"`), measures D89's five criteria again with both solvers,
and decides on the numbers, not on ems's account of them. Developed against ems's source before the release; the
released crate's `src` is that source byte for byte, and the 60 mm line was run again on it. The table is in
ELECTRICAL.md, "ems as a second solver". No openEMS, CSXCAD or other GPL code was read; nothing was changed in
ems.
**Adopted in the adapter** (`fdtd::ems`):
- `CompileOptions::with_pole_weighting(PoleWeighting::Averaged)`: an edge in a laminate's surface gets the Debye
  poles by the share of its dual face the laminate fills, as it gets the static ε, and as the built-in solver
  does (`engine.rs` averages a cell's Debye terms by area with its ε). ems's default is openEMS's rule, the whole
  poles for such an edge, which is right for running openEMS's files and was the two points of D89's long line.
- The check for a negative energy at the end of a run is gone: ems ends such a run as `Diverged` itself
  (`sim.unstable`, as before).
- Nothing for the end criterion: `RunOutcome::energy` is now the criterion's measure (the fields, without what
  a conductor model holds inside), and the adapter only passes it on as the run's `energy_db`.
`sim_sparams_microstrip_validation` holds ems to the built-in solver's 6 % again (8 % in D89). New:
`measure_memory`, one run per process (`CADLAB_MEM_CASE`, `CADLAB_MEM_SOLVER`, `CADLAB_MEM_END`,
`CADLAB_MEM_THREADS`), for memory and for a time taken alone.
**The criteria, and what each measured** (release builds, three threads allowed, the built-in solver's default
thread count within them; times vary by a fifth on this shared machine):
1. *Every validation tolerance of the built-in solver's tests holds with ems.* Met. The long line (60 mm of
   `solid` copper, 10 GHz, |S₂₁| against D52 within 3 % to 2.5 GHz and 6 % above): the built-in solver −2.3 to
   +4.9 %, ems −1.3 to +5.3 % (0.0.9: −1.4 to +7.2 %), +4.9 / +4.5 / +5.3 % where 0.0.9 was over; Z₀, εeff and
   |S₁₁| as before. Passivity next to the absorbing layer: Σ|S|² at most 1.0007 (the built-in solver 1.0010, of
   1.0025). The solder mask: Z₀ 50.62 Ω, εeff 3.1542 (50.61 Ω, 3.1541). Parts: |S₂₁| within 0.0094 of theory
   (the built-in solver 0.0116, of 0.02). The 45° line: 154.7° and 157.7° (154.8°, 157.5°), and 141.1° against
   141.0° on its 2.5 million cells. Session 6: |S₂₁| 0.065 and |S₁₁| 0.094 from the ideal chain (of 0.1 and
   0.15). The cavities and the lumped fixture as csx models read as in D89.
2. *No case where ems is materially worse in accuracy.* Met. The long line is 0.4 points from the built-in
   solver at 9 GHz (0.005 dB of 1.74 dB; two points before). Slanted `solid` copper: the conductor's part of
   |S₂₁| on the bent line 0.2 to 0.9 % below the built-in solver's on 0.25 mm cells and 1.2 to 2.2 % below on
   the default mesh (10 to 14 % above before); sheets 0 to 4.5 % above. The oracle's lines: the conductor's part
   within 0.0001 dB (`solid`) and 3 to 4 % (sheets), |S₂₁| within 0.005 dB (sheets) and 0.010 dB (`solid`,
   lumped ports; 0.006 dB before: the laminate's rule again, the copper's part is equal). The via transition:
   0.018 dB at 4 GHz with conformal cells (0.012 dB before), 0.003 dB as staircases. The dielectric loss alone
   against D52: ems −2.5 to +7.1 %, the built-in solver −5.3 to +9.0 %.
3. *Run time within 1.5 times.* Met on what D89 named, lossy `solid` copper with vias, and on every case with
   `solid` copper: the via transition to −40 dB 53 s against 126 s, to −60 dB (the default) 75 s against 204 s,
   in 48 993 / 49 025 steps against 49 920 (0.0.9: 69 313); the oracle's `solid` line to −70 dB 35 s against
   107 s in 42 721 steps against 42 400 (72 769); the bent line with `solid` copper to −70 dB 77 s against 91 s
   on 0.25 mm cells and 12 s against 51 s on the default mesh, 13 665 and 13 729 steps against 13 408 and
   13 440 (57 697, 58 305); the region of 25 194 cells with a via 24 s on three threads against 35 s, and 50 s
   on one (1.45 times; 0.0.9: 102 s). With lossy sheets: the oracle's line 17 s against 45 s, the bent line on
   the default mesh 8 s against 24 s, session 6 105 s against 83 s (1.26 times). **Not met on one case:** the
   bent line with lossy sheets on 0.25 mm cells to −70 dB, 58.6, 62.7 and 64.8 s against 34.1, 40.1 and 34.8 s
   (one run per process, alternating): 1.8 times, in the same steps (7809 against 7616; 0.0.9 took 22 593 and
   was four times). On one thread it is 92 s against 78 s and on two 74 s against 45 s: ems gains a factor of
   1.5 from three threads there and the built-in solver 2.2, so more threads would widen it.
4. *Deterministic for any thread count.* Met: identical Touchstone data on one, two and three threads with
   conformal cells (the via transition with lossy `solid` copper, the bent line with lossy sheets) and on one
   and four (lossless, lossy sheets, lossy `solid` copper).
5. *No panics, no instabilities.* Met: every run of the table ends, Σ|S|² at most 1.0006 with perfect copper
   and 0.998 with lossy copper (`measure_diagonal_copper` asserts it for every copper model on both meshes).
Memory, not a condition (D89), one run per process: the via transition 136 MB against 34 MB, the oracle's
`solid` line 65 MB against 31 MB, the bent line on 0.25 mm cells 158 MB against 58 MB.
**D89's condition (c)**: the oracle job ran on master with ems 0.0.9 and passed on its three cases for both
solvers (the via transition within 0.06 dB of openEMS for ems and 0.05 dB for the built-in solver). It has not
run with 0.0.10. To watch there: on the sheet line ems 0.0.9 was the solver nearer openEMS (its largest
relative difference 30 % where the built-in solver's is 41 %, both at the lowest frequencies, where the absolute
term of the test's 15 % + 0.025 dB carries them), because openEMS and ems 0.0.9 share the rule for the poles of
a surface edge;
with the averaged poles ems reads what the built-in solver reads there (0.005 dB apart), so its table should
now look like the built-in solver's and pass as that one does.
**Decision.** The default does not change, and `src/fdtd/engine.rs` stays. Four criteria are met and D89's (a),
(b) and (c) with them; criterion 3 is not met on one case of D89's table, 1.8 times where 1.5 is the limit, and
the limit is not moved for it. That case is not what `sim.sparams` runs unless asked (`copper: sheet` with a
`max_cell` of half the default on slanted copper), and with the default copper ems is the faster solver by 1.2
to 4 times; it is still a model a user can ask for where the default would get slower by switching, with
session 6 (1.26 times, also sheets with diagonal tracks) pointing the same way. Switch when ems's lossy sheets
with slanted outlines run in its threads on such a mesh (within 1.5 times of the built-in solver's 35 s on
`CADLAB_MEM_CASE=bent CADLAB_MEM_END=70dB measure_memory`), and the oracle job has passed with 0.0.10.
**The switch is prepared**, on the branch `ems-010-default`, one commit on top of this one: the default `Solver` is
ems in a build with the cargo feature `ems` and the built-in one in a build without it (where `solver: ems`
keeps answering `sim.solver_unavailable`), the output and the Touchstone header always name the solver, and the
tests cover both builds. `ems` is already a default feature (D86), so nothing changes for cargo-deny (ems and
csx are MIT, no further dependencies) or the MSRV (both declare 1.95).
*Not established:* what keeps ems's threads from helping with lossy sheets and conformal cells on the fine mesh
(perfect sheets with conformal cells take it 23 s there and lossy sheets as a staircase 31 s; ems's design notes
say the sheets' surface edges run on one thread where its conformal cells act between the half-steps, and that
cadlab's bent line on the default mesh has none that do; on the fine mesh ems reports 10 of 177 cut faces
interpolated from 30 others, none merged, which may be that case and was not followed further); which solver is
nearer a
finer mesh on the via transition, now 0.018 dB apart; the time at the thread count an unshared machine would
give (ems up to one per physical core, the built-in solver one per 131 072 nodes: this was measured with three
allowed); the oracle with ems 0.0.10; the via transition on 0.2 mm cells, not run again.

### D91. guide 0.0.4 adopted: no fill at the end, the smallest via for escapes, and half the time budget kept for a second run on the routed board; the default unchanged (2026-11-15)
guide 0.0.4 is the release for a host that gives a run a budget and fills its zones itself, which is cadlab with
its 60 s (guide's `docs/benchmarks.md`, "PCB routing with a budget"). `guide = { version = "0.0.4",
default-features = false, features = ["pcb"] }`; nothing else in `Cargo.lock` moves.
**What the adapter takes from it** (`router::guide`; ROUTER.md, "guide as a backend"):
- `RouteOptions::fills = false`. The adapter never read guide's fills: the tracks and vias come back, the count
  and every reported connection are read from the board cadlab fills again (D80), and `Stats::islands` goes to a
  progress line only. guide then skips its last fill.
- `RouteOptions::fanout_via`: the smallest via the rules allow, `min_drill` with `min_annular_ring` around it.
  It is the native fanout's small via, now one function for both (`router::smallest_via`). guide uses it for the
  dog-bone of a trapped pad only where the net's own via fits nowhere near.
- `CopperZone::plane` stays `None`. guide no longer takes a narrow zone of a net of four pads or fewer for a
  plane. The board has no word for plane or pour, so cadlab knows nothing guide's rule does not, and saying
  `Some(true)` for every zone (guide's rule before) is worse where it was tried: corne-cherry 86 open against
  54, lumenpnp-mobo 30 against 9 to 12.
- *Causes.* `pcb::Cause` has the six variants of 0.0.3; none is new. What changed is when `Unknown` comes: the
  pours between a few pads that guide joined as planes and then reported apart with no cause are copper now, and
  no connection of the twelve corpus boards has `Unknown` at 60 s. The reason for it, kept for that variant and
  any later one, no longer says that guide "took the net for routed": `guide left <pad> apart from <pad> on net
  N and gives no cause for it`, with the net as subject and the hints to route the net again with guide or with
  the native router. `NoPath` after a run its deadline stopped says that whether the pad is shut in was not
  looked at, because guide does not look then.
- *Budgeted and unbudgeted runs differ by design.* With a work budget or a deadline guide routes for the most
  connections soonest, whether the budget stops it or not. D88's "the same result for the same input" holds
  with the budget as part of the input: the same work budget gives the same result on any machine, the same
  time budget the same result as long as it does not cut the run, and neither gives the result of a run with no
  budget. The unit test that expected a large work budget to reproduce the unbudgeted run now expects it to
  complete and to repeat.
**The second run after a cut run.** D88 skipped the second run after a run a budget stopped, and gave the first
four fifths of the time budget. On lumenpnp-mobo at 60 s the first run was then cut 18 s into guide's own
joining of its zones' pieces, which is done on a fill of guide's that cadlab does not use, and 48 pads stayed
off their pours. Now the first run has half the budget; the verification and refill after it are timed; the
second run has until the end of the budget less one and a half times that and half a second, and is not started
with less than a second. After a run its deadline stopped it takes every net the board has open, not only those
with zones, less the nets guide tried and could not finish: `Routed::unrouted` of a stopped run lists every net
with a zone (it counts no fill), so those are read from the causes, a net with a connection whose cause is
neither `Stopped` nor `Unknown`. Not after a run the work budget stopped: nothing is left of it, and guide's
planning costs nothing in that unit. Tried and not kept: four fifths with the second run in what remains
(lumenpnp-mobo 50: guide's joining runs 7 s past the deadline and no time is left), three fifths (58: the
second run ends inside guide's planning of its stubs, 16 s that are not stopped half way, and returns nothing),
half with the second run for nets with zones only (635 on the twelve boards against 589 to 600).
**Measured** (ROUTER.md, "Native router and guide 0.0.4, D91"; the corpus at 60 s; *before* is guide 0.0.4
behind D88's adapter, run twice with the same counts; the native router's column is the one measured for
guide's record of 0.0.4, seed 1, not run again). Open connections, native / before / after:
buspirate-flash-sop, buspirate-rs232 and lumenpnp-ringlight 0 / 0 / 0; nrfmicro 0 / 11 / 11; sweep-v2.2 3 / 6 /
6; corne-cherry 1 / 90 / 54; tinytapeout-demo 6 / 47 / 45; buspirate5-rev10 2 / 17 / 14; lumenpnp-mobo 10 / 48 /
9; cynthion 28 / 32 / 32; glasgow-revC3 35 / 69 / 69; glasgow-revD1 no result / 394 / 349. Twelve boards: 714
before, 589 after (guide 0.0.3: 1023). The eleven with a native result: 85 / 320 / 240. Zero DRC errors other
than unrouted, nothing taken out by the verification. The cost of the half: lumenpnp-mobo's first run is cut
before its cleanup, 795 vias where it had 461. A cut run ends where the clock finds it, so these move by a few
connections with the machine's load (cynthion 32 and 34, lumenpnp-mobo 9 and 12 in two runs).
**What got in the way, in guide** (for its next release):
1. *Planning is not stopped.* The "escapes and planes" phase of a run does not look at the deadline: 15.6 s on
   lumenpnp-mobo's second run and 27.5 s on cynthion's (18 nets), against 3.1 s and 5.5 s for the first run of
   the same board, the routed tracks being fixed copper to measure the fills against. cynthion's call took
   62.3 s of 60, and a run stopped in that phase returns nothing, so the time is lost. `CADLAB_BENCH_ROUTER=guide
   CADLAB_BENCH_TRACE=1 CADLAB_BENCH_BUDGET_MS=60000 route_bench --corpus cynthion`.
2. *The joining of zones' pieces is slow and ends late.* On lumenpnp-mobo it runs from 38 s to 55.6 s, 7.6 s
   past the 48 s deadline, with `fill_on_stop` and `fills` both off (its fill is asked to stop between the
   layers of a zone, by guide's documentation). A second run of the adapter does the same work in its planning,
   on cadlab's fill. A host that fills its zones itself would rather have the joining left out, or done on
   `Zones::fills` given again.
3. *No cause for the pad after a timed stop.* `Cause::NoPath` is given without the enclosure test when a
   deadline or the flag stopped the run, though its documentation says the pad is not shut in nearby:
   glasgow-revC3 has 66 pads shut in when its run ends and 1 when it is cut, with 68 "found no way". The
   blockers are what the hints are made of.
4. *`Routed::unrouted` of a stopped run* with `fill_on_stop` off lists every net with a zone. Which nets the
   run tried and failed has to be read from the causes.
5. *Vias in pads with `vias_in_pads` off*, by the bench's box test (a via's center in the box of a
   surface-mount pad of its net): 4 of 513 on buspirate5-rev10, 4 of 502 on corne-cherry, 7 of 875 on
   glasgow-revD1, 3 of 795 on lumenpnp-mobo; the DRC has nothing against them. Not looked into.
6. *Still open from D88:* a fine-pitch pad a track of the net's width does not leave (nrfmicro 11 open with
   `fanout_via` and without, glasgow-revC3 69, tinytapeout-demo 45), and the time: glasgow-revD1 is cut with 343
   connections not yet tried.
**Default: unchanged.** The condition is that guide be at least as good as the native router on every board. At
the same 60 s it is level on three, one connection ahead on lumenpnp-mobo, and behind on seven, 240 open against
85 on the eleven boards with a native result. `router: guide` stays the second opinion.
*Why:* the second run is where the adapter's own time goes, and it is the only part of the run that works on
the copper cadlab will check and count; a budget that a first run spends whole leaves the pads its routes cut
off as they are. Half is not derived, it is what the corpus measured better than four fifths and three
fifths, and it should move when guide's planning can be stopped or its joining left out.


### D93. guide 0.0.5 adopted: its joining of zones' pieces left out, the stopped connections told apart by `Routed::tried`, the half of the budget and the second run measured again and kept; the default unchanged (2026-11-15)
guide 0.0.5 answers D91's list of what got in the way: every phase keeps the deadline, a budget is shared out
between routing, cleanup and the joining of zones' pieces, a pad that is shut in says so after a stopped run,
and `Routed::tried` names the nets that had a search (guide's `docs/benchmarks.md`, "PCB routing in parts").
`guide = { version = "0.0.5", default-features = false, features = ["pcb"] }`; nothing else in `Cargo.lock`
moves.
**What the adapter takes from it** (`router::guide`; ROUTER.md, "guide as a backend"):
- `RouteOptions::join = false`, in both runs. The joining is guide filling the zones around its routes and
  joining what that fill leaves apart; cadlab fills again itself and gives guide what its own fill leaves
  apart, in the second run. Measured off and on, the corpus at 60 s has the same count within what two runs
  differ by (567 to 576 against 564 and 586), as guide's record says. It is off for what else it does: the
  small boards take half the time, lumenpnp-mobo's first run has the time for its cleanup (about 690 vias
  where it had 770), and a run with a work budget is not stopped by it. That last is a fault of guide 0.0.5
  found here: with `join` on, a work budget of 20 M nodes and a board with zones, the run says the work budget
  was spent after 13 000 to 50 000 nodes and leaves a connection open on each of buspirate-flash-sop,
  buspirate-rs232 and lumenpnp-ringlight, which 0.0.4 completes and 0.0.5 with `join` off completes.
- `Routed::tried`, for the reason of a connection the stop left open: a net not in it was never got to (`the
  time budget ended before guide got to net N: <pad> is apart from <pad>`, a larger budget or fewer nets at a
  time), a net in it had its turn and was without a way when the run stopped (`… before guide had routed net N
  to <pad> again: the net had its turn, and was without a way there when the run stopped`, a larger budget or
  the native router). guide's summary of a run in the progress says how many nets it got to.
- *Not* `tried` for the second run's nets. D91 read the nets a stopped run tried and could not finish from the
  causes, a net with a connection whose cause is neither `Stopped` nor `Unknown`. With `tried` in its place,
  every net that had its turn left out, the twelve boards have 651 open where they have 564 to 586, and seven
  boards get no second run: a stopped run has given nearly every net its turn (glasgow-revD1 all 561), and
  what is open is mostly nets that another net's route left without a way, which the run on the routed board
  finds a way for (glasgow-revD1 399 against 349, corne-cherry 62 against 51 to 54). What the second run must
  leave out is the nets a search *failed* for, and that is the cause. So the rule stays, and `unrouted` of a
  stopped run, which no longer lists every net with a zone, is still not read after a stop.
- *No wording of its own for `NoPath` after a deadline.* guide looks at a pad's surroundings after a stopped
  run too: glasgow-revC3, cut, has 65 pads shut in and 4 "found no way" (0.0.4: 1 and 68), with the blockers
  as subjects and hints.
- *A run that comes back before its deadline can be one that was stopped*, since routing ends at three
  quarters of the time: `budget_exhausted` and `route.budget_cut` follow guide's `stopped` as before, so they
  are set for such a run. That is right for the user (the budget is why connections are open) and needs no
  change.
**The half, measured again.** guide now keeps a quarter of what a run is given for after routing, so the
first run's share was a question again. With `join` off: two fifths 572, half 571 and 573, three fifths 577,
two thirds 593, three quarters 636; with `join` on, half 586 and 564, two thirds 587, four fifths 663. Level
from two fifths to three fifths and worse above, because a second run on a large board plans for 8 to 13 s
before it routes ("escapes and planes" on the routed board with cadlab's fills). Half stays. **The second
run, measured again:** one run with nine tenths of the budget leaves 681. It is still what joins pads to
cadlab's fill (lumenpnp-mobo: a first run that ends on its own reports every connection made, and the
refilled board has 51 open) and what routes the nets a cut run left (glasgow-revD1 399 against 344 to 350).
Not started after a run the work budget stopped, as before: allowed one with what the first left of the
budget, the four boards a budget of 20 M stops had the same result to the via.
**route_bench's via-in-pad count** tests the pad's copper (`polyclip::locate` on the pad's shape), not its
box, for both routers: 0 on every corpus board for guide 0.0.4, guide 0.0.5 and the native router, where the
box had 3 to 7 of guide's on four boards (D91, item 5), each beside a round pad in the corner of its box.
**Measured** (ROUTER.md, "Native router and guide 0.0.5, D93"; the corpus at 60 s, each row of guide twice;
*before* is guide 0.0.4 behind D91's adapter, the native router run here, seed 1, three threads). Open
connections, native / before / after: buspirate-flash-sop, buspirate-rs232 and lumenpnp-ringlight 0 / 0 / 0;
nrfmicro 0 / 11 / 8; sweep-v2.2 3 / 6 / 6; corne-cherry 3 / 54 / 54; tinytapeout-demo 8 / 50 and 53 / 45;
buspirate5-rev10 2 / 14 / 16; lumenpnp-mobo 15 / 59 and 12 / 6 and 7; cynthion 28 / 34 and 36 / 23;
glasgow-revC3 34 / 69 / 69; glasgow-revD1 201 / 350 and 349 / 349 and 344. Twelve boards: 647 and 604
before, 576 and 572 after (D91 recorded 589 for the same *before*; guide's own record of 0.0.5 has 607 and
567: the machine was shared each time, and the spread of *before* is lumenpnp-mobo's second run, which had no
time left in one run of the two). The eleven without glasgow-revD1: 93 / 297 and 255 / 227 and 228. Zero DRC
errors other than unrouted, nothing taken out by the verification, no call of guide over 60 s after (62.2
and 65.1 s before). guide 0.0.5 behind D91's adapter unchanged: 586 and 564, so the release is the gain and
the adapter's part is what `join` off gives beside the count.
**Gaps, for guide's next release**, most useful first:
1. *A work budget with `join` on stops a run that has spent a thousandth of it* (above;
   `CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_WORK=20000000 route_bench --corpus buspirate-rs232` at cadlab
   `d564baf` with guide 0.0.5: "49888 nodes; the work budget was spent", one connection open; the second run
   of the adapter the same after 365 nodes with nothing open).
2. *The nets a search failed for, as a list.* `tried` has every net that had its turn; a host that routes
   again wants those whose last search ended with no way (as against those another net's route left without
   one when the run stopped), which is what `unrouted` is after a run that ended. Read from the causes here.
3. *The plan of a run on a routed board*: "escapes and planes" takes 8 to 13 s on lumenpnp-mobo, cynthion and
   glasgow-revD1 for a second run of 1 to 135 nets, before any routing, and it is why the second run needs
   half the budget. A plan for the nets to route only, or one kept from the first run, would let the first
   run have more.
4. *Rip-up that does not settle*, guide's own finding: 343 of glasgow-revD1's open connections and 18 of
   corne-cherry's 54 are nets that had their turn and were without a way at the stop, and corne-cherry's other
   36 are pads shut in, most by the routes of the pads beside them; the native router leaves 201 and 3 there
   in the same time.
5. *Fine-pitch pads*, still: 161 pads shut in at 60 s (glasgow-revC3's array 65, tinytapeout-demo, nrfmicro 8
   open), D88's first item.
6. *A first run's own account of its zones is not the host's*: lumenpnp-mobo's run reports 524 of 524
   connections and the refilled board has 51 open. Harmless with the second run; it means `Stats::islands`
   and `open` of a run with `fills` off cannot be shown to a user as the board's.
**Default: unchanged.** The condition is that guide be at least as good as the native router on every board.
At the same 60 s it is level on three, ahead on two (lumenpnp-mobo 6 and 7 against 15, cynthion 23 against
28) and behind on seven, 227 open against 93 on the eleven boards and 344 to 349 against 201 on glasgow-revD1.
`router: guide` stays the second opinion.
*Why:* each release of guide has taken a workaround out of the adapter, and this one takes the reason for a
special wording and the time lost after a deadline; what it adds that the adapter does not use as offered
(`tried` for the second run, `join` on) was measured before it was left, so the next reader does not have to
wonder whether it was overlooked.

### D94. ems 0.0.11, and `sim.sparams` runs on ems by default: D90's one case of run time measured again and met; `solver: builtin` kept (2026-11-15)
D90 ended on one case of its third criterion: the bent line with lossy sheets on 0.25 mm cells to −70 dB took
ems 1.8 times the built-in solver's time, where 1.5 is the limit, and the switch waited on the branch
`ems-010-default`. ems 0.0.11 was written against that case. This change adopts it (`ems = "0.0.11"`; nothing
changes in the adapter), measures criterion 3 again on every case of D90's table with both solvers, checks that
the other four still hold, and switches the default. The table is in ELECTRICAL.md, "What ems 0.0.11 changed".
Measured on ems's source before the release, through a path that was not committed; the released crate's `src`
is that source byte for byte, and the blocking case was run again on it (35.5 s against 30.6 s). No openEMS,
CSXCAD or other GPL code was read; nothing was changed in ems. The limit and the tolerances are D89's and
D90's, unmoved.
**How it was measured.** Release builds, one run per process (`measure_memory`, which now takes the rest of
D90's cases and can keep its Touchstone file; session 6 through `rf_path_full_wave_ems`, which takes a thread
count), the two solvers alternating, three runs each for the blocking case and session 6 and two for the
others, on a shared machine with three threads to use. D90's times were taken with each solver's own thread
count within three, which for the built-in solver is one thread on a region under 262 144 nodes; here both
solvers were also given `threads: 3`, which is the harder comparison for ems on small regions, and the two
cases that decide were run both ways.
**Criterion 3, run time within 1.5 times** (built-in against ems, seconds):
- *The blocking case*, both on three threads: 40.9, 47.1, 32.7 against 37.7, 37.5, 29.5 (0.8 to 0.9 times;
  ems 0.0.10 the same way: 56.7). With each solver's own count, which is three for both on 578 680 cells: 38.6,
  46.2, 34.0 against 37.7, 31.8, 32.9. On one thread 73.7 against 78.5. The same steps as before (7616 and
  7809). Met.
- *Session 6* (lossy sheets with diagonal tracks on a graded mesh, to −50 dB), both on three threads: 47.7,
  46.8, 53.4 against 62.7, 60.8, 58.3: 1.31, 1.30 and 1.09 times in the three pairs, 1.34 the slowest of ems
  over the fastest of the built-in solver. With each solver's own count, the built-in solver's one thread
  (152 280 cells) and ems's three, as a run without `threads` goes: 78.8, 77.7, 75.8 against 66.5, 63.7, 58.5,
  0.8 times. Met both ways. D90's 1.26 times (105 s against 83 s) was the second kind; with three threads each
  0.0.10 took 102.8 s against 54.8 s, 1.9 times, which D90 did not measure and which would not have met the
  limit.
- *The rest of D90's table*, both on three threads: the bent line with `solid` copper on 0.25 mm cells 67 against
  45 (0.0.10: 60), on the default mesh 17.6 against 7.5 with sheets and 34 against 12 with `solid` copper; the
  oracle's line 34.5 against 14.6 with sheets and 66.8 against 30.6 with `solid` copper; the via transition 80
  against 42 to −40 dB and 128 against 60.6 to −60 dB, `sim.sparams`'s default; the via on 25 194 cells 34
  against 18 to 20 with ems on three threads, and 34.3 to 35.0 against 43.7 to 46.1 with one thread each, 1.27
  to 1.34 times (1.45 with 0.0.10). Met on every case.
**The other four**, as far as they were checked again:
1. *Every validation tolerance.* The `#[ignore]` validations that take a solver were run on both solvers with
   ems 0.0.11 from crates.io and pass at the built-in solver's tolerances: `sim_sparams_microstrip_validation`,
   `lossless_line_near_the_pml_is_passive`, `sim_sparams_mask_validation`, `sim_sparams_parts_validation`,
   `routed_45_degree_line`, and session 6's `rf_path_full_wave`, which ran on the default solver alone and now
   names both.
2. *No case materially worse in accuracy.* Nothing to measure again: 0.0.11's Touchstone files are 0.0.10's
   byte for byte on the nine cases compared (the bent line with lossy sheets and with lossy `solid` copper on
   both meshes, the oracle's two lines, the via transition to −40 and −60 dB, the small via region), so D90's
   numbers stand.
4. *Deterministic for any thread count.* The same bytes on one and three threads (the bent line with lossy
   sheets on both meshes) and on one, two and three (the small via region with lossy `solid` copper).
5. *No panics, no instabilities.* Every run above ended converged; `measure_diagonal_copper` (every copper model
   on both meshes, each asserted to end and to stay passive) passes.
**Decision.** `sim.sparams` runs on ems by default. The switch is D90's prepared commit: the default `Solver` is
ems in a build with the cargo feature `ems` (a default feature) and the built-in one in a build without it,
where `solver: ems` keeps answering `sim.solver_unavailable`; `solver: builtin` runs cadlab's own solver, and
`src/fdtd/engine.rs` stays. For a user: the output's `solver` is always there (it was left out for the built-in
solver), the text result says "with ems:" or "with the built-in solver:", and the Touchstone file's first line
says `(solver: ems)` or `(solver: builtin)` (nothing, for the built-in solver, before); the numbers of a run
that names no solver move by what the two solvers differ, which is the table's last column (session 6: |S₁₁|
0.5726 for 0.5733 and |S₂₁| 0.7988 for 0.7983 at 2.44 GHz; the oracle's lines 0.005 and 0.010 dB; the via
transition 0.018 dB at 4 GHz); ems's own warnings (`sim.ems_<code>`) can appear; a run takes more memory and,
without `threads`, more threads. No golden file or snapshot changed: none holds a result of `sim.sparams`, and
the `describe` schema of the command is 10 972 bytes of the 11 000 its test allows.
**Tests.** Nothing is tested only as the default: every board-level test names its solver and runs on both
(D89's list, and `rf_path_full_wave` from here on); `sim_sparams_command` and `ems_runs_the_region` check which
solver a run without `solver` gets, in a build with the feature and in one without, and that it writes the file
`solver: ems` writes. The openEMS oracle compares openEMS with both solvers by name, so the switch changes
nothing in it.
**Open, and not conditions of this decision:**
- *Memory.* ems takes 1.6 to 4.4 times the built-in solver's (the process's peak: the via transition 150 MB
  against 35 MB, the bent line on 0.25 mm cells 160 MB against 58 MB, on the default mesh 55 MB against 35 MB).
  0.0.11 did not change it. D89 set memory aside as a condition; it is now what a default run costs.
- *One thread with a Debye laminate.* 1.3 times the built-in solver's time on the region of 25 194 cells
  (ems's own figure for small regions: 1.2 to 1.5 times). Within the limit, and not what a run without
  `threads` does on a machine with more than one core.
- *Session 6 at equal threads*, 1.1 to 1.3 times: the Debye poles on a graded mesh (ems's notes).
- *The built-in engine* is not removed (D86's last step): it is the second result, the solver of a build
  without the feature, and what ems is measured against.
*Not established:* times on an unshared machine with more cores, where ems takes up to a thread per core of the
pool (a thread per five x planes and 4096 cells) and the built-in solver one per 131 072 nodes: every time here
was taken with three threads allowed, and they vary by a fifth between runs (the built-in solver's blocking case
from 32.7 to 47.1 s); the openEMS oracle with ems 0.0.11, which runs in CI only (with 0.0.10 on master it
passed for both solvers: the via transition within 0.07 dB for ems and 0.05 dB for the built-in solver, the
lines' largest relative difference 36.0 and 40.6 % for ems and 40.6 and 42.0 % for the built-in solver, at the
lowest frequencies where the test's absolute term carries them; 0.0.11 should read the same to the digit, its
results being 0.0.10's); the via
transition on 0.2 mm cells, not run again; which solver is nearer a finer mesh on the via transition (D90).


### D95. guide 0.0.6 adopted: after a cut run the second run takes every open net, `Routed::failed` leaves nets out only after a run that ended, the first run has 0.45 of the budget; the second run and `join` off measured again and kept; the default unchanged (2026-11-16)
guide 0.0.6 is the release that plans the ways out of fine rows and deep balls in a run with a budget, moves a
net aside before it takes one out, and answers two items of D93's list: a work budget with `join` on no longer
stops a run, and `Routed::failed` names the open nets whose own search found no way (guide's
`docs/benchmarks.md`, "PCB routing with ways out planned"). `guide = { version = "0.0.6", default-features =
false, features = ["pcb"] }`; nothing else in `Cargo.lock` moves.
**What the adapter takes from it** (`router::guide`; ROUTER.md, "guide as a backend"):
- *`Routed::failed`, and where it is not used.* D93 read the nets a stopped run tried and could not finish
  from the causes and left them out of the second run. `failed` is that list from guide: with it in place of
  the causes, and the first run at half the budget as it was, the twelve corpus boards have 420 and 420
  connections open at 60 s, with the causes 421, board by board the same but for one connection. Measured
  beside them, the second run with no net left out: 401 and 411, and 411 and 419 in two more runs
  (tinytapeout-demo 39 to 41 against 46 and 47, buspirate5-rev10 6 against 7, cynthion 21 to 24 against 25; the
  eleven boards without glasgow-revD1 174 to 181 against 188 and 189). A search that failed in a run that was
  cut is not the last word on the routed board, and the second run has the time: with the failed nets left out
  it came back on its own with 9 to 26 s of the budget unused on five boards. So after a run that its deadline
  stopped the second run takes every net the board has open, and `failed` is what it leaves out after a run
  that *ended*, where `unrouted` was read before: that run had the time it wanted, and nothing has changed for
  those nets. With every open net taken after a run that ended too (nrfmicro, 4 nets): the same 7 open.
- *The first run has 0.45 of the budget*, where it had half since D91. With the second run taking every open
  net: a third 397, two fifths 366 and 369, 0.45 356 and 350 (and 353 and 359 as committed), half 411 and 419,
  three fifths 499. Nine boards are the same from two fifths to half within what two runs differ by, and two
  decide. glasgow-revD1 has 183 with a third, 167 to 177 with two fifths, 171 to 178 with 0.45, 227 to 238
  with half and 321 with three fifths: its first run has as many connections open at 30 s as at 27 s, 327,
  while its second, which plans for 10 s before it routes, makes some ten a second for as long as it has.
  cynthion has 32 with a third, 31 to 36 with two fifths, 20 to 26 with 0.45 and 21 to 24 with half: its
  second run plans for 8 to 15 s, and below 0.45 the first has too little. It is a share between two edges
  that one board each sets, on a machine that was shared, and it should move with guide's plan on a routed
  board. (With `failed` leaving nets out after a cut run, measured first: a third 411, two fifths 458, half
  420 and 420, three fifths 498, two thirds 513, three quarters 512.)
- *`join` stays off.* The fault D93 found is gone: with `join` on and a work budget of 20 M nodes,
  buspirate-rs232, buspirate-flash-sop and lumenpnp-ringlight complete and are not stopped (174 935, 94 587
  and 11 441 nodes; `CADLAB_BENCH_ROUTER=guide CADLAB_BENCH_WORK=20000000 route_bench --corpus
  buspirate-rs232`). Measured on, at 60 s with the first run at half: 432 open against 420, cynthion 31 against
  25, corne-cherry 51 against 47, glasgow-revC3 41 against 37, tinytapeout-demo 42 against 46, lumenpnp-mobo
  14 against 13; and the three small boards take 0.9 to 1.8 s where they take 0.4 to 0.9 s. The joining is
  still work that cadlab's fill and the second run do again.
- *The run's own words.* The line of a run in the progress has the nets with no way of their own
  (`Routed::failed`) and how many times a net was moved aside (`Stats::shoved`).
**The second run, measured again:** one run with nine tenths of the budget leaves 598: lumenpnp-mobo 59,
corne-cherry 98 (33 of them pads its routes cut off from their pours), lumenpnp-ringlight 9, glasgow-revD1
309, one each on buspirate-flash-sop and buspirate-rs232. It stays. Only cynthion is better without it, 16.
**Measured** (ROUTER.md, "Native router and guide 0.0.6, D95"; the corpus at 60 s, each row of guide twice,
one thread a run and three runs at a time; *before* is guide 0.0.5 behind D93's adapter, master `323ba6b`; the
native router run here, seed 1, three threads). Open connections, native / before / after:
buspirate-flash-sop, buspirate-rs232 and lumenpnp-ringlight 0 / 0 / 0; nrfmicro 0 / 8 / 7 and 8; sweep-v2.2 3 /
6 / 6; corne-cherry 3 / 54 / 46 and 49; tinytapeout-demo 8 / 45 and 42 / 39 and 40; buspirate5-rev10 2 / 16 / 6;
lumenpnp-mobo 15 / 7 / 13; cynthion 27 / 23 and 22 / 22 and 26; glasgow-revC3 35 / 69 / 37; glasgow-revD1 201 /
344 / 177 and 174. Twelve boards: 294 / 572 and 568 / 353 and 359 (guide's own record of 0.0.6 behind D93's
adapter: 409 and 412; that adapter with 0.0.6 here: 421). The eleven without glasgow-revD1: 93 / 228
and 224 / 176 and 185. Zero DRC errors other than unrouted, nothing taken out by the verification, no via in a
pad, no call over 59.0 s. With a work budget of 20 M nodes, where no share of time comes into it, the twelve
boards give the same result to the via and the tenth of a millimetre in two runs.
**What the connections cost** is vias and track, as guide's record says. Against 0.0.5: corne-cherry 581 vias
and 7433 mm where it had 462 and 5833 (a quarter more of each), glasgow-revC3 587 and 4126 mm against 418 and
3174 (two fifths and three tenths more), glasgow-revD1 1067 and 7838 mm against 873 and 6066 (a fifth and
three tenths more, for 167 more connections); buspirate5-rev10 has fewer vias, 437 against 512. Against the
native router: lumenpnp-mobo 731 vias against 344, tinytapeout-demo 479 against 273, glasgow-revC3 587
against 413, buspirate5-rev10 437 against 313, cynthion 645 against 478, corne-cherry 581 against 483,
glasgow-revD1 1067 against 1008; the track is longer by 4 to 23 % on five of them and shorter on corne-cherry
(7433 against 8276 mm, with 43 more connections open) and on glasgow-revD1 (7838 against 8785 mm, with 24
fewer open). The table in ROUTER.md has all three per board.
**lumenpnp-mobo, 13 open where 0.0.5 leaves 7.** Looked into with the first run at half, for guide: it is
the order of the nets together with the fanout via, and no setting of the adapter mends it without a loss
elsewhere.
- With no seed (shortest first) the first run has 189 of 192 nets at 9.4 s and then searches for one,
  `Microcontroller/PE10` (U11.23 to U5.41), until 27.8 s. Routing's share ended at about 23 s (three quarters
  of what was left at 2.1 s), so cleanup has 2.2 s where 0.0.5 gave it 7 s, the run is cut, and the board keeps
  731 vias and the pads of U5, U8 and U9 that the tracks beside them shut in. 0.0.5's first run ends on its own
  at 29.2 s with every net.
- With seed 1 and seed 2: 6 and 6 open (0.0.5: 7 and 8), the first run ending on its own at 28.8 and 15.2 s.
- Without `RouteOptions::fanout_via` (the smallest via the board's rules allow, 0.4 mm on a 0.3 mm drill), no
  seed: the first run ends at 18.7 s with all 524 connections, and 7 are open, twice; 4 and 10 with seeds 1 and
  2. The result with the fanout via has no escape via in it, so what the via changes on this board is in the
  plan, not in what comes back. Without it on the whole corpus: 434 against 420, glasgow-revC3 54 against 37
  and corne-cherry 53 against 47, so it stays.
- Not `neck_down` (off: 13 again), not `join` (on: 14), not the first run's share (0.45: 13; three fifths to
  three quarters: 10, with the first run ending on its own at 34.9 s).
**Gaps, for guide's next release**, most useful first:
1. *Routes through a host's fill are counted as if the fill stayed.* Still D93's item 6, and the largest
   thing the adapter works around: a first run counts on the fill it planned on, its routes cut pads off that
   fill, and a second run has to find them a way on a board that is full by then. corne-cherry's one run of
   54 s says 34 open and the refilled board has 98, 33 of them pads off their pours; lumenpnp-mobo's 13 are
   GND and +3.3V pads of fine-pitch parts shut in by the tracks of the pads beside them. A pad's way to its
   fill kept free while other nets are routed, or the count and the joining made on the host's fills cut by
   the routes, would take the second run's reason away.
2. *The plan of a run on a routed board*, D93's item 3, is cheaper and still what fixes the share: 10 to 11 s
   on glasgow-revD1, 8 to 15 s on cynthion, 7 to 10.5 s on lumenpnp-mobo before any routing, three runs at a
   time (0.0.5: 8 to 13 s). And the second run is where glasgow-revD1's connections are made: a first run cut
   at 24, 27 or 30 s leaves 345, 327 and 327 open, and the run on that board with its copper fixed takes them
   to 167 to 178 in 14 to 18 s of routing, or to 232 in 11 s. A first pass that did the same, without the stop and the plan in
   between, would not need the host's second run for it.
3. *One net can hold routing past its share* (lumenpnp-mobo above: 18 s on one net, 4.8 s over), and what
   `fanout_via` changes on a board with no array to fan out. `RAYON_NUM_THREADS=1 CADLAB_BENCH_ROUTER=guide
   CADLAB_BENCH_TRACE=1 CADLAB_BENCH_BUDGET_MS=60000 route_bench --corpus lumenpnp-mobo` at this commit.
4. *`failed` after a cut run is not final.* tinytapeout-demo's first run names 26 nets; given to the second
   run with the rest, 5 to 7 more connections are made (140 of 178 by guide's count, the nets kept in part),
   cynthion 1 to 4. A host cannot tell from the list which of them a run on the returned board has room for;
   here they are all tried.
5. *A first run that goes on makes the board worse for the second*, on corne-cherry: 4 nets with no way of
   their own at 30 s, 21 at 44 s, 29 at 54 s, while its own count of open connections goes from 45 to 42 to
   34. Rip-up that does not settle, guide's own finding, seen from the host.
6. *Vias and track* (above): a quarter to two fifths more than 0.0.5 on corne-cherry and glasgow-revC3, and
   twice the native router's vias on lumenpnp-mobo and tinytapeout-demo.
7. *Fine-pitch pads*, still the most of what is open: of 353, 243 pads shut in (124 of them on
   glasgow-revD1), 50 connections of nets that had their turn and were without a way at the stop, 25 "found no
   way", 28 at a search's node limit, 7 pads the refilled pour does not reach. tinytapeout-demo's QFN under
   its area rule is as it was, 39 against the native router's 8.
8. *Time on the boards that end*: nrfmicro 25 to 28 s for 7 open where 0.0.5 took 13 s for 8;
   buspirate-rs232 needs a second run now (one net open on the filled board) and takes 0.9 s for 0.4 s.
**Default: unchanged.** The condition is that guide be at least as good as the native router on every board.
At the same 60 s it is level on three, ahead on three (glasgow-revD1 174 and 177 against 201, lumenpnp-mobo
13 against 15, cynthion 22 and 26 against 27) and behind on six: glasgow-revC3 37 against 35, buspirate5-rev10
6 against 2, sweep-v2.2 6 against 3, nrfmicro 7 and 8 against 0, tinytapeout-demo 39 and 40 against 8,
corne-cherry 46 and 49 against 3. 176 and 185 open against 93 on the eleven boards. `router: guide` stays the
second opinion.
*Why:* the release is most of the gain (572 to 420 with the adapter as it was), and the adapter's part is
what the measurements moved: a list offered for the second run was tried as offered, found equal to what it
replaces, and then found to cost connections after a cut run, which is where it would have been used most;
and the half, which three releases had left where it was, is no longer where the corpus is best.
*Not measured:* the share with seeds, or on a machine that is not shared (the edges it sits between are set
by seconds of guide's plan); the generated boards with guide 0.0.6 through the adapter (guide's record has
them, all complete); budgets other than 60 s and 20 M nodes; the native router with more than 60 s; cadlab's
oracles.


### D96. guide 0.0.7 adopted: a courtyard's clearance rule goes over whatever other kinds it names, a rule for a footprint's pads as the pads' own clearance; the first run's share and the second run measured again and kept; the default unchanged (2026-11-16)
guide 0.0.7 is the release whose plan is sooner: a host's fills are cut into their parts once, a fill is cut
round new copper in a window, and global routing ends when it gains nothing (guide's `docs/benchmarks.md`, "PCB
routing with ways out planned", the part on 0.0.7). Its API is that of 0.0.6. `guide = { version = "0.0.7",
default-features = false, features = ["pcb"] }`; nothing else in `Cargo.lock` moves.
**The gap in the adapter**, found by guide's side on tinytapeout-demo. The board's rules (its `.kicad_dru`,
imported as custom rules) allow 0.16 mm for what is in the QFN-80's courtyard on the front and is no zone, and
after that 0.18 mm for the part's pads; the board's clearance is 0.2 mm, its supplies and analog nets have 0.24
and 0.25 mm, and the pads are 0.2 mm apart. D61's adapter took a rule as an area only when it named no kind of
item, and named every other rule in `route.guide_custom_rules` as left to the verification. Both rules were
left out, guide routed at the classes' clearances, and no track of a supply net, nor of the pad beside one,
left the part: 29 of the 46 connections open were such pads.
**How the three resolve a rule.** The DRC (`board::clearance`) gives each item the last rule in board order
whose scope holds it (kind, layer, footprint, courtyard or area), and a pair the later of its two items' rules,
in place of local and class clearances, never under `min_clearance`. The native router resolves the same for
copper that does not exist yet (`router::scoped`: existing items by `Clearances`, new copper by where it is, a
track split where its rules change), so it honours these rules already: tinytapeout-demo's 8 open, 6 in the
run here, are with them. guide has two places: a rule area, whose clearance is between two pieces of copper
that both touch it, in place of any other, the first area first; and a pad's own clearance, which replaces the
rules' between it and anything.
**What goes over now** (`router::guide::custom_rules`; ROUTER.md, "guide as a backend"):
- *A rule is an area's* when its scope is `in_courtyard` or `touches_area` alone, with or without a layer, its
  kinds take in tracks and vias, and it is between the scope and anything or the scope and itself. Every new
  piece of copper in the area is then in the scope, and the DRC applies a rule to a pair when either item is
  in it, so which other kinds the rule names (pads, drawings, not zones) changes nothing for a route. A rule
  for tracks alone or vias alone stays out.
- *A rule's clearance is the own clearance of the pads and copper drawings it is the last to hold*, in place
  of their local one: what a rule for the pads of a footprint says, and what an area's rule asks between a pad
  in it and a track outside it, where guide's area says nothing. A pad stack has one clearance: the larger of
  its shapes' rules, and its local one when a shape has no rule.
- *An area yields to nothing in guide, and to a later rule in the DRC.* So each area, each courtyard on its
  own, has the largest of its rule's clearance, the clearances of the existing copper in it that a later rule
  holds (zone fills apart, which cadlab fills again), and those of later rules that can hold a new track or via
  and whose courtyards or areas meet its box. A pad's own clearance is raised the same way by such rules within
  1 mm of it. tinytapeout-demo: the QFN's courtyard 0.18 mm and its 81 pads 0.18 mm, the two carrier
  connectors' courtyards and 116 pads in them 0.16 mm (two of other parts), the USB connector's courtyard and pads 0.2 mm. guide's
  side proposed the first half of this for one case (a later rule for the pads of the same footprint pattern
  raises the courtyard's area); here it is from the items the DRC's own resolution says a later rule holds,
  so a pad of another part in the courtyard counts too.
- *Named in `route.guide_custom_rules`* only the rules that can hold new copper and are no area (tracks or
  vias alone, `in_area`, both positional terms, two different scopes) and `disallow` rules. A rule for
  footprints or for kinds that are neither track nor via is given whole and no longer named.
- *Not looser than the DRC* but in what the verification is for, as before: a rule that is named above and
  tightens the clearance, a track long enough to be in two areas far apart, copper that touches an area's
  outline and reaches in by less than the DRC's micrometre. *Stricter than the DRC:* two tracks in the QFN's
  courtyard keep 0.18 mm where the DRC asks 0.16, since the area's clearance replaces the pads' own; a track
  in an area and one outside keep their classes'.
- *Checked:* `a_courtyard_rule_and_a_rule_for_the_pads_of_the_footprint` (five pads 0.2 mm apart on a board at
  0.25 mm: three shut in without the rules; with them one area at 0.18 mm, the pads at 0.18 mm, all five
  routed, nothing taken out, `drc::check_copper` clean on the result and not clean with the rules removed; the
  native router routes all five too), and on the corpus no run of the final adapter with anything taken out
  for a clearance.
**The first run's share**, 0.45 since D95, measured again because a second run now plans for 3 to 4 s where it
took 8 to 15 (glasgow-revD1 3.3 s, cynthion 3.1 to 4.3 s, lumenpnp-mobo 2.9 to 3.3 s from the call to the
first search). Connections open on the twelve boards at 60 s, summed over three orders of the nets (no seed,
seeds 2 and 3): 0.35 920, 0.4 901, 0.45 890, half 971, 0.55 955, 0.6 1019, 0.65 1159, 0.7 1189. Two things
pull apart. glasgow-revD1's first run gains nothing from more time and its second makes connections for as
long as it has: 427, 432, 454, 550, 552, 640, 783, 821 along the row. The eleven others want the first run
longer: 493, 469, 436, 421, 403, 379, 376, 368 (cynthion 90, 73, 73, 70, 51, 47, 42, 40; tinytapeout-demo 58 to
41; corne-cherry 136 to 109). 0.45 has the least sum and stays. It is one board's edge against eleven boards'
slope, and a first run of guide's that came back once it had stopped gaining would let the share go.
**The second run** is still worth having: one run with nine tenths of the budget leaves 571, 569 and 480 over
the three orders, 1620 against 890 (lumenpnp-mobo 61, 52, 46 against 10, 5, 7; corne-cherry 97, 88, 74 against
45, 37, 35; glasgow-revD1 309, 313, 251 against 168, 136, 150; the three small boards 11 or 12 pads off their
pours against none). cynthion is again better with one run, 17, 17 and 23.
**Measured** (ROUTER.md, "Native router and guide 0.0.7, D96"; the corpus at 60 s, three orders of the nets,
one thread a run and three runs at a time; *before* is guide 0.0.6 behind D95's adapter, master `c85fc3c`, run
again here; the native router run here, seed 1, three threads). Open connections on the twelve boards over the
three orders: before 412, 417, 374; guide 0.0.7 behind D95's adapter 338, 294, 298; after 315, 275, 300 (the
first order again: 306); the native router 285. The eleven without glasgow-revD1: 184, 186, 173; 173, 158, 168;
147, 139, 150; native 93. tinytapeout-demo 40, 40, 37; 40, 35, 44; 14, 20, 20 (and 11 twice more); native 6.
Zero DRC errors other than unrouted on every run, no via in a pad of its net, no call over 59.0 s, nothing
taken out by the verification in the four runs of *after*.
**What moves between two runs.** D95 recorded 353 and 359 for the adapter that has 412 here in the same
order: glasgow-revD1 177 and 174 then, 228 now, on a machine with a load of 7 to 12 on 64 cores from other
work. A run that a deadline cuts moves with the machine: glasgow-revD1 by fifty, 168 and 157 in two runs of
*after* in a row; the other boards by up to four (buspirate5-rev10 11 and 15, tinytapeout-demo 14 and 11).
Between orders of the nets the twelve boards differ by forty. What is said here is what holds over the three
orders.
**Vias and track**, first order, guide after / native: lumenpnp-mobo 734 vias and 6330 mm / 347 and 5424 mm;
tinytapeout-demo 568 and 5560 mm / 274 and 4913 mm (482 and 4994 mm before, with 26 more connections open);
buspirate5-rev10 477 and 3523 mm / 313 and 3053 mm; cynthion 612 and 3765 mm / 476 and 3177 mm; glasgow-revC3
573 and 4060 mm / 415 and 3436 mm; corne-cherry 609 and 7606 mm / 483 and 8276 mm; glasgow-revD1 1162 and
8124 mm / 1046 and 8967 mm; sweep-v2.2 42 and 2231 mm / 21 and 2161 mm. Twice the native router's vias on
lumenpnp-mobo, tinytapeout-demo and sweep-v2.2, a quarter to a half more on four, a ninth more on
glasgow-revD1. The table in ROUTER.md has all of it per board.
**Gaps, for guide's next release**, most useful first:
1. *A first run that ends when it has stopped gaining.* glasgow-revD1's first run has 327 connections open
   at 21 s, 321 at 27 s, 310 at 33 s and 309 at 42 s, and the host's second run on that board makes 150 more
   in 24 s; every other board wants the first run longer. The share is a host's guess at what the run itself knows. With it gone the eleven
   boards would have some 60 fewer open over three orders (436 to 376) and glasgow-revD1 none more.
2. *Routes through a host's fill*, D95's item 1, as it was: one run leaves 97 on corne-cherry and 61 on
   lumenpnp-mobo where two leave 45 and 10, and the three small boards 11 pads off their pours.
3. *An area's clearance that does not replace a pad's own larger one*, or a clearance between an area's
   copper and named pads. tinytapeout-demo's courtyard has to be given at 0.18 mm for its pads' sake, and
   its tracks then keep 0.18 mm from each other where the board allows 0.16.
4. *Rule areas by kind and by containment.* A rule for tracks alone, or for copper wholly inside an area
   (KiCad's `enclosedByArea`; tinytapeout-demo's two breakout areas at 0.16 mm), has no place: an area takes
   all copper that touches it. And *either* item in the area, as KiCad and cadlab's DRC apply a rule: guide's
   area needs both, so a track in the courtyard keeps its class's clearance from one outside.
5. *An area's track width in routing.* `Overrides::track_width` is checked and not routed by: tinytapeout-demo
   allows 0.16 mm tracks in the courtyards, and its supplies' pads are 9 of the 14 it has open (+1V1, +3V3:
   U6.5, U6.10, U6.24, U6.29, U6.51, U6.64 and the capacitors beside them). Their class is 0.25 mm wide, and
   such a track out of a pad 0.2 mm wide is 0.175 mm from the pads beside it, under the 0.18 mm. Whether
   `neck_down` reads an area's clearance was not looked into.
6. *A via on a via.* On corne-cherry a second run put a via of GNDR where the board it was given has one of
   the same net (`drc.hole_to_hole`, 0 mm apart), and the verification took the piece out, 43 items: once with
   0.0.6 (seed 2) and once with 0.0.7 (no seed, the first run at 0.4), in thirty-four runs of the board.
7. *Fine-pitch pads shut in*, still the most of what is open: of 315 in the first order 199 pads shut in, 46
   that the budget ended before, 34 at a search's node limit, 22 "found no way", 14 pads the refilled pour does
   not reach. corne-cherry 35 to 45 against the native router's 3.
8. *Vias*, above: twice the native router's on three boards.
9. *buspirate5-rev10* has 11 and 9 open in two of three orders with 0.0.7 where 0.0.6 has 6 and 6 (5 and 5 in
   the third); 11 and 15 in two runs of the first order.
**Default: unchanged.** The condition is that guide be at least as good as the native router on every board.
At the same 60 s, over the three orders against the native router's one run: level on three
(buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight); ahead on two in every order (glasgow-revD1 168,
136, 150 against 192; lumenpnp-mobo 10, 5, 7 against 15); within what runs differ by on two (cynthion 19, 22,
32 against 28; glasgow-revC3 34, 37, 40 against 36); behind on five in every order (buspirate5-rev10 11, 6, 5
against 2; sweep-v2.2 6, 7, 6 against 3; nrfmicro 8, 5, 5 against 0; tinytapeout-demo 14, 20, 20 against 6;
corne-cherry 45, 37, 35 against 3). 147, 139 and 150 open against 93 on the eleven boards; on the twelve 315,
275 and 300 against 285, one order of three ahead. `router: guide` stays the second opinion.
*Why:* the rules were the board's own and both other resolvers had them; the adapter's reading of "an area's
rule" was narrower than what guide's area means for a route. The share was measured because its reason, the
second run's plan, had changed, and the measurement says the reason left is one board.
*Not measured:* the native router over three seeds; the share on a machine that is not shared; the generated
boards with guide 0.0.7 through the adapter (guide's record has them, all complete); budgets other than 60 s;
a work budget; cadlab's oracles; KiCad's DRC on a board guide routed under the rules.

### D97. guide 0.0.8 and 0.0.9 adopted: the first run has 0.7 of the budget and the nets with zones in it, the pads cut off their fills joined by guide, the rows planned left off; the default unchanged (2026-11-17)
guide 0.0.8 answers the first of D96's gaps another way than it was asked. A net that found no way in its corridor
is searched for with nothing moved and keeps what that reaches, as a net routed in part, before any net crosses
another's routes; a run joins the pads that its routes cut off the fills it planned on again, by stubs, once routing
is over (`RouteOptions::pour_cuts`); two tracks out of two pads side by side in a rule area are necked by the area's
clearance; and the ways out of a row of fine pitch can be planned row by row (`RouteOptions::row_fanout`, off by
default) (guide's `docs/benchmarks.md`, "PCB routing with a second wait"). guide 0.0.9, of the same day, is 0.0.8
without the via on a via of D96's list. `guide = { version = "0.0.9", default-features = false, features = ["pcb"]
}`; nothing else in `Cargo.lock` moves.
**What the adapter takes from it** (`router::guide`; ROUTER.md, "guide as a backend"):
- *The first run has 0.7 of the budget*, for 0.45 (`FIRST_SHARE`). D96's reason for 0.45 was one board:
  glasgow-revD1's first run made nothing more after some 20 s and a second run on the routed board made 150
  connections, while the eleven other boards wanted the first run longer. What the second run did was what a board
  where nothing moves makes cheap: of the board's 561 nets, 94 were still waiting with nothing of their 313
  connections when the first run was cut at 27 s, behind a few searches that cross other nets' routes and cost up to
  their limit of nodes each, and a search with nothing to cross ends soon. guide 0.0.8 makes those searches first,
  in the first run: it has 182 of 1948 connections open at 27 s where 0.0.7 has 325, and the second run then makes
  15 where it made 111. The board is the same with either share now, and the eleven others decide. Connections open
  on the twelve boards at 60 s, summed over three orders of the nets (no seed, seeds 2 and 3): 0.0.7 with 0.45 975,
  and in an hour in which it had 851, 1190 with 0.7 (glasgow-revD1 424 and 821, the eleven others 427 and 369);
  0.0.8 with 0.45 917, with 0.7 783 (glasgow-revD1 465 and 422; the eleven 452 and 361). Between them, in a build of
  0.0.8's source that is not committed: 0.45 863, 0.6 786, 0.7 781 and 0.8 821. And with the zones' nets in the
  first run, below: 0.45 908, 0.7 760.
- *The nets with zones are in the first run, open or not* (`first_nets`). The adapter gave guide the nets of the
  board's open connections. lumenpnp-mobo's ground has none before routing: cadlab's fill of its F.Cu pour joins
  every pad of it. So the first run was not asked for the ground, its routes went through that pour, and the second
  run, on the routed board, was asked for some 220 stubs to the ground's plane at once, of which 5 to 10, pads of
  the LQFP-100 and of the two drivers, were shut in by then: all of what the board had open (9, 10 and 6), where
  guide's first run counted itself complete, 524 of 524. With the nets that have a zone among those of the first
  run, guide plans the ground on its plane on an inner layer and gives every pad its stub while the board is free:
  813 of 813 connections in 21 s for 524 of 524 in 33 s, 0, 1 and 0 open over the three orders, 713 vias for 748.
  The other boards are as they were (their nets with zones had open connections already): 783 to 760 on the twelve.
  A net out of scope (`route.nets`) is not added.
- *`pour_cuts` stays on*, guide's default, set by name. With it the eleven boards without glasgow-revD1 have 358
  open over three orders and 381 without (corne-cherry 106 and 118, tinytapeout-demo 22 and 28), in the uncommitted
  build with the first run at 0.7.
- *`row_fanout` stays off*, guide's default, set by name: 367 with the rows planned for 358 (corne-cherry 99 for
  106, nrfmicro 15 for 18; glasgow-revC3 127 for 113, buspirate5-rev10 14 for 11).
- *The supply pins of tinytapeout-demo's QFN*, D96's item 5 in part: 0.0.8 necks two tracks side by side in an area
  by the area's clearance, so the courtyard given at 0.18 mm lets the pins of +3V3 and +1V1 leave beside their
  neighbours at 0.22 mm. 13, 20 and 20 to 21, 14 and 6 open with nothing changed in the adapter, 9, 8 and 5 after.
- *A via on a via*, D96's item 6, was guide's and is gone with 0.0.9. A track that ends at a via is two of its net's
  places at one point, and guide asked the first of the two which layers the net joins there: from the track's layer
  the track's end, which joins none. A path found from its far end is laid the other way round, and was given a via
  of its own on the one it went through: a second run on a board with the first run's stubs, as here. guide now asks
  all the places at the point. In 21 runs of seven corpus boards the old question would have had another answer 12
  times, mostly where the run's own via was (one via in the result); the verification took such a piece out twice in
  D96's runs and once here (23 items of GNDR on corne-cherry, in an uncommitted build with the first run at 0.45).
  Nothing changes in the adapter, and the verification stays.
**Measured** (ROUTER.md, "Native router and guide 0.0.8, D97"; the corpus at 60 s, three orders of the nets, one
thread a run and six runs at a time, guide 0.0.8 and 0.0.9 from crates.io; *0.0.7* is master `ecf0242`, D96's
adapter, run again here; *after* is guide 0.0.9 behind the adapter of this change; the native router run here, seed
1, three threads, one run at a time). Open connections on the twelve boards over the three orders: 0.0.7 354, 278
and 343; 0.0.8 behind D96's adapter 330, 302 and 285; 0.0.8 with the first run at 0.7 285, 261 and 237; 0.0.8 behind
the adapter of this change 276, 253 and 231, and 273, 255 and 235 in a second run; after 273, 250 and 238 (again:
272, 250 and 239); the native router 294. The eleven without glasgow-revD1: 141, 142 and 155; 163, 145 and 144; 123,
125 and 113; 114, 117 and 107; 111, 114 and 114; native 93. Zero DRC errors other than unrouted on every run, no via
in a pad of its net, no call over 58.8 s, nothing taken out by the verification in any of these runs.
**What moves between two runs.** The same binary of 0.0.7 had 1096 and 851 open over three orders two hours apart on
this machine, with a load of 8 to 14 from other work (glasgow-revD1 629 and 424), and 975 here; D96 recorded 890. A
run that a deadline cuts moves with the machine, and glasgow-revD1 with 0.0.7 by a hundred in one order. With 0.0.8
the same runs repeat nearly to the connection (760 and 763 in two runs of 0.0.8 behind the new adapter, 761 and 761
in two of *after*, 781 and 780 in two of the uncommitted build before the zones): on the boards that the budget
cuts, a pass that ends with fewer connections than its routes made at their best goes back to those, and the best is
reached before the cut. What is said here is of runs of the same hour.
**Vias and track**, first order, guide after / native: lumenpnp-mobo 713 vias and 6544 mm / 347 and 5424 mm;
tinytapeout-demo 605 vias and 5799 mm / 274 and 4913 mm; buspirate5-rev10 501 vias and 3816 mm / 313 and 3053 mm;
cynthion 631 vias and 3911 mm / 476 and 3177 mm; glasgow-revC3 541 vias and 3919 mm / 415 and 3436 mm; corne-cherry
825 vias and 8436 mm / 483 and 8276 mm; glasgow-revD1 1085 vias and 7954 mm / 1008 and 8784 mm; sweep-v2.2 42 vias
and 2231 mm / 21 and 2161 mm; nrfmicro 62 vias and 499 mm / 46 and 454 mm. The table in ROUTER.md has all of it per
board.
**The generated boards** through the adapter of this change with 0.0.9, one run each with the bench's 120 s: all
eight complete with no DRC error, and the same routes with 0.0.8 (attiny-2l 0 vias in 0.02 s, soic24-2l 159 in 1.2
s, stm32-2l 72 in 1.4 s, stm32-4l 51 in 1.1 s, qfp-qfn-4l 172 in 4.4 s, fine-4l 105 in 3.1 s, bga144-4l 248 in 13.6
s, big-4l 1994 vias and 21 789 mm in 70 s).
**Tried and not kept**, in uncommitted builds, each over the three orders at 60 s:
- *Runs that move what the runs before placed*: after the two runs, guide's `eco::reroute_with` with the new copper
  within 1.5 mm of the ends of what is still open taken out and routed again, in another order of the nets each
  time, a run on the board filled again after each, kept when the board has fewer open, for as long as there is
  time. nrfmicro 12 open for 18 and buspirate5-rev10 9 for 11; nothing on lumenpnp-mobo and tinytapeout-demo, where
  a run that took out the copper round 5 to 11 open pads left 50 to 70 open when it was cut (a net with no zone that
  touches an area is taken out whole). At 3 mm and 4.5 mm no run of any board gained. Eight connections for four
  more runs of guide and ninety lines of adapter.
- *A lattice of half the QFN's pitch* (`grid` 0.2 mm for guide's 0.22 mm on corne-cherry, as the native router snaps
  its grid): 114 for 110.
- *The ground as tracks where its pours cover little* (`plane_coverage` 0.25 and 0.6 for 0, D88's question again on
  corne-cherry): 108 and 110 for 110.
- In guide, for the pads that routes cut off their pour (its `docs/benchmarks.md`): a pad beside another net's made
  a terminal of its own though the fill joins it (glasgow-revC3 677 for 113), and the pads of rows alone given a
  stub through a via before anything is routed (343 for 337 on the eleven boards).
**Gaps, for guide's next release**, most useful first:
1. *A way out of the pads of a fine row that holds while routes are moved.* corne-cherry has 36, 35 and 38 open
   against the native router's 3: of 36 in the first order, some 20 are pads of its two RP2040s (QFN-56, 0.4 mm) and
   its USB receptacles with free room straight ahead and a pocket beyond it, closed by the tracks and vias of the
   pads beside them, which fan out in front. Each of those nets routes alone in a second. guide's rows planned as
   routes (`row_fanout`) are cut like any other route, and planned as walls they were worse (its record). The native
   router plans a package's escapes together and keeps them.
2. *A pour on an outer layer that routes shred.* corne-cherry's ground is a pour on both of its two layers and
   nothing else: 16 of its 36 open are pads of GND and GNDR on pieces of pour that the routes of 150 nets cut off,
   on both layers, where a stub through a via reaches another piece that is cut off too. guide's stubs after routing
   join what still has a way; planning the ground as tracks (`plane_coverage`) did not gain.
3. *Rip-up that settles, and the time left.* nrfmicro's first run ends on its own after 25 s with 4 to 9 open and 34
   s of the budget unspent (the USB receptacle's D+ and D− pads, which the native router gives a crossover via, and
   pins of the module shut in by other nets' routes); glasgow-revD1 makes 11 connections in the last 15 s of
   its first run and 8 in its second.
4. *Searches on a wide board.* sweep-v2.2 has 6 or 7 open against 3: 43 nets on 2 layers, 27 M nodes in 41 s with no
   net moved, its ground at a search's node limit and its supply waiting when the run is cut.
5. *Rule areas by kind and containment, with a track width that is routed by*, D96's items 3 to 5 as they were but
   for the necking: tinytapeout-demo has 9, 8 and 5 against 6, 5 or 6 of them pins of +3V3.
6. *Vias*: twice the native router's on lumenpnp-mobo, tinytapeout-demo and sweep-v2.2, 1.3 to 1.7 times on
   nrfmicro, glasgow-revC3, cynthion, buspirate5-rev10 and corne-cherry, 1.08 on glasgow-revD1.
**Default: unchanged.** The condition is that guide be at least as good as the native router on every board. At the
same 60 s, over the three orders against the native router's one run: level on three (buspirate-flash-sop,
buspirate-rs232, lumenpnp-ringlight); ahead on three in every order (glasgow-revD1 162, 136 and 124 against 201;
lumenpnp-mobo 0, 1 and 0 against 15; cynthion 19, 14 and 19 against 28); within what runs differ by on three
(tinytapeout-demo 9, 8 and 5 against 6; buspirate5-rev10 2, 6 and 3 against 2; glasgow-revC3 35, 36 and 34 against
36); behind on three in every order (nrfmicro 4, 7 and 9 against 0; sweep-v2.2 6, 7 and 6 against 3; corne-cherry
36, 35 and 38 against 3). 273, 250 and 238 open against 294 on the twelve boards, ahead in every order; 111, 114 and
114 against 93 on the eleven without glasgow-revD1, behind in every order by 18, 21 and 21, where corne-cherry alone
is behind by 33, 32 and 35: the ten others together are ahead by 15, 11 and 14. `router: guide` stays the second
opinion.
*Why:* the share was a host's guess at what the run knew, and the run now does first what made the guess necessary.
The nets with zones were left out by a reading of "open" that is right for the count and wrong for a router, which
cuts what a fill joins. Both were found by looking at what a run did second by second, not at what it left.
*Not measured:* the native router over three seeds; a machine that is not shared; one run alone against two with
0.0.8; the generated boards more than once; budgets other than 60 s; a work budget; cadlab's oracles; KiCad's DRC on
a board guide routed.

### D98. ems 0.0.12: the three things D94 left open on ems's side measured again; memory 1.3 to 1.8 times the built-in solver's, the Debye laminate on one thread 1.1 times, session 6 at equal threads faster; the built-in engine stays until the build without `ems` is decided (2026-11-17)
D94 switched `sim.sparams` to ems and left the built-in engine in place, with three things open on ems's side
(memory 1.6 to 4.4 times the built-in solver's, a Debye laminate on one thread 1.3 times its time on a small
region, session 6 at equal threads 1.1 to 1.3 times) and two things to see first (the openEMS oracle job with
ems as the default, and a release with it). ems 0.0.12 was written against the three, with csx 0.0.3 for one of
them. This change adopts it (`ems = "0.0.12"`), measures every case of D90's table again with both solvers, and
says where the removal stands. The table is in ELECTRICAL.md, "What ems 0.0.12 changed". Measured on ems's
source before the release, through a path that was not committed; the released crate's `src` is that source byte
for byte, and the bent line with lossy sheets and the small via region on one thread were run again on it, with
the same bytes (the built-in solver's 50.7 s, a build running beside it, against 30.0 s; 34.8 s against 37.0 s).
No openEMS, CSXCAD or other GPL code was read. The limit and the tolerances are D89's and D90's, unmoved.
**What changed in ems** (its design notes, §4.1 and §6, wave 27; no result changes, by its own tests and by the
files below): the update coefficients are held as their distinct rows where the cells repeat (they were twelve
arrays, 48 bytes per node; on a mesh uniform in the plane most rows repeat); csx's material
arrays are taken one direction at a time (111 bytes per node at the peak of a compile, and a second
discretisation for the mask of lossy metal); a round of the conformal cells is the list of the values it sets,
and its stability estimate runs on six `f64` per node for ten; a laminate of Debye poles holds a charge per pole
and two constants per pole of each distinct edge (24 bytes a pole on a graded mesh for 84, and its loop reads
24 bytes a pole and writes 8 where it read 40 and wrote 16); the surface impedance reads the coefficients where
they are and lets its request lists go; a probe's sample times are the rule they follow; nothing is allocated on
threads that end (glibc keeps such memory with the thread's arena, and the second port's engine took new).
**Adopted in the adapter** (`fdtd::ems`): a port's run is `Simulation::run_into_results`, which takes the
simulation and moves the probes' series into the results. `run_with` copies them, and the engine was alive
beside the copy until the port's spectra were formed. Nothing else: the compile options, the model and the
reading of the waves are D90's.
**How it was measured.** As D94: release builds, one run per process (`measure_memory`: the process's peak and a
time taken alone), the two solvers alternating, two runs each, on a shared machine with three threads to use,
both solvers with `threads: 3` unless said; session 6 through `rf_path_full_wave_ems`, three runs each way. ems
0.0.11 was run once the same day on every case for its peak and its Touchstone file. The machine was busier than
for D94: the built-in solver's times with three threads on its small regions are up to two fifths over D94's
(the oracle's `solid` line 93 to 95 s for 67 s), ems's are D94's or under.
**The three open points.**
- *Memory* (the process's peak, 22 MB of each before the run; built-in against ems 0.0.12, with 0.0.11 in
  brackets): the bent line on 0.25 mm cells 57 MB against 91 to 97 MB with sheets (159) and 72 against 111 to
  113 with `solid` copper (185); on the default mesh 34 against 49 (54) and 38 to 39 against 49 to 50 (66); the
  oracle's lines 29 against 41 to 42 with sheets (60) and 31 against 48 to 49 with `solid` copper (74); the via
  transition 34 to 35 against 58 to 62 (134 to −40 dB, 148 to −60 dB); its small region 29 to 30 against 47 to 49
  (87). 1.3 to 1.8 times, where it was 1.6 to 4.4. Not at the built-in solver's level, and by ems's account of
  what is left (two `f64` vectors over the grid while a port's engine is built with conformal cells, the
  coefficients on a mesh graded in the plane, a value per step and probe) not about to be.
- *One thread with a Debye laminate* (the via on 25 194 cells, to −30 dB): 34.7, 34.0 s against 37.8, 38.0 s,
  1.1 times (36.2 s on ems's source two hours earlier; 43.3, 44.2 s with 0.0.11 the same day, 1.3 times). With
  three threads each 44.9, 34.4 s against 16.1, 14.8 s, and as a run without `threads` goes (the built-in solver
  one thread, ems three) 34.5, 33.8 s against 15.9, 15.3 s.
- *Session 6 at equal threads* (three each): 51.4, 59.6, 51.4 s against 42.9, 44.6, 42.7 s, 0.7 to 0.9 times
  (D94: 47.7, 46.8, 53.4 against 62.7, 60.8, 58.3, 1.1 to 1.3 times). With each solver's own count, the built-in
  solver's one thread and ems's three: 76.7, 75.8, 76.2 s against 45.2, 42.4, 42.8 s. The results are D94's
  (|S₁₁| 0.5726 for 0.5733 and |S₂₁| 0.7988 for 0.7983 at 2.44 GHz).
**D90's criteria again.**
1. *Every validation tolerance.* The `#[ignore]` validations that take a solver were run on both solvers with
   ems 0.0.12 from crates.io and pass at the built-in solver's tolerances: `sim_sparams_microstrip_validation`,
   `lossless_line_near_the_pml_is_passive`, `sim_sparams_mask_validation`, `sim_sparams_parts_validation`,
   `routed_45_degree_line` and session 6's `rf_path_full_wave`; so do the tests of `tests/fdtd_ems.rs`,
   `tests/fdtd.rs`, `tests/fdtd_parts.rs`, `tests/fdtd_conformal.rs` and `tests/agent_session6.rs` that are not
   ignored.
2. *No case materially worse in accuracy.* Nothing to measure again: 0.0.12's Touchstone files are 0.0.11's
   byte for byte on the nine cases compared (the bent line with lossy sheets and with lossy `solid` copper on
   both meshes, the oracle's two lines, the via transition to −40 and −60 dB, the small via region), so D90's
   numbers stand.
3. *Run time within 1.5 times* (built-in against ems, seconds, three threads each): the bent line on 0.25 mm
   cells 37.3, 35.2 against 28.1, 26.0 with sheets (D90's blocking case, 0.7 to 0.8 times; on one thread 74.3,
   73.5 against 60.3, 64.2) and 77.9, 78.2 against 37.8, 37.0 with `solid` copper; on the default mesh 19.8, 18.2
   against 8.3, 7.9 and 45.0, 37.0 against 13.0, 12.0; the oracle's lines 44.8, 37.5 against 15.0, 14.3 and 94.6,
   93.3 against 28.2, 28.3; the via transition 87.0, 110.2 against 35.6, 37.5 to −40 dB and 153.2, 173.7 against
   54.0, 51.6 to −60 dB. Met on every case; the one row where ems is the slower solver is the small via region
   on one thread, 1.1 times.
4. *Deterministic for any thread count.* The same bytes on one and three threads (the bent line with lossy
   sheets on both meshes and with lossy `solid` copper on 0.25 mm cells) and on one, two and three (the small via
   region).
5. *No panics, no instabilities.* Every run above ended converged; `measure_diagonal_copper` (every copper
   model on both meshes, each asserted to end and to stay passive) passes, Σ|S|² at most 1.0006 with perfect
   copper and 0.998 with lossy copper.
**The two things to see first.**
- *The openEMS oracle job with ems as the default.* It ran on master with ems 0.0.11 as the default (run
  43939785789) and on the branch `ci/ems-012` with 0.0.12 (run 43964289403), and passed both times, nine tests
  each. With 0.0.12 the lines' largest relative difference from openEMS is 36.0 % (sheets) and 41.2 % (`solid`
  copper) for ems and 40.6 and 42.6 % for the built-in solver, at the lowest frequencies, where the absolute
  term of the test's 15 % + 0.025 dB carries them; the via transition is within 0.07 dB for ems and 0.05 dB for
  the built-in solver. With 0.0.11: 36.0 and 40.2 %, 40.6 and 41.6 %, 0.07 and 0.05 dB. The `solid` line moved
  by a point for both solvers between the two runs, with cadlab's files the same bytes: that is openEMS's run,
  not ems's.
- *A release with it.* ems 0.0.12 and csx 0.0.3 are on crates.io (ems's tests on Linux, Windows and macOS, its
  docs and its MSRV job green on the release), and cadlab depends on them from there. cadlab's own release
  with it is the merger's.
**Decision.** ems 0.0.12 is adopted, and the built-in engine is not removed by this change. Of what D94 named,
the time on one thread and session 6 are no longer reasons to keep it, the oracle job has run with ems as the
default, and memory is 1.3 to 1.8 times where it was 1.6 to 4.4: a cost that is stated, not a defect. What
still holds it is D94's second reason, and it is a decision, not a measurement: the built-in engine is the only
solver of a build without the cargo feature `ems`. Removing it means one of
- *`ems` always there:* the feature goes and ems and csx become dependencies of every build (both MIT, no
  further dependencies, 21 s of a release build, MSRV 1.95 as cadlab's). `sim.sparams` then has one solver in
  every build and `solver` has nothing to choose.
- *The feature stays, and a build without it has no full-wave solver:* `sim.sparams` answers
  `sim.solver_unavailable` there, as `solver: ems` does now, and `export.openems` remains for such a build.
- *The built-in engine stays for that build only:* nothing is removed, and its tests have to run in a build
  that CI does not make.
With either of the first two, what goes with the engine is the second result on the same model (the tests that
hold each solver to the other, and the answer to "which of the two is off" that a second solver in the same
process gives; the openEMS oracle stays, in CI only), `solver: builtin` in the command's schema, and
`fdtd::conformal`, the built-in solver's own cut cells. What does not go: the region, the mesh, the ports and
the waves (`fdtd::extract`, `place_ports`, `Waves`), which are both solvers', and the types the adapter takes
from `fdtd::engine` (`Gaussian`, `Hooks`, `LumpedPort`, `Probe`, `EPS0`), which would move.
**Tests.** No test changed: every board-level test names its solver and runs on both, and none holds a number
that 0.0.12 moves. `measure_memory`'s doc names D98.
*Not established:* times on an unshared machine with more cores (D94); what the built-in solver's runs with
three threads on small regions cost on a quiet machine (here 93 to 95 s where D94 read 67 s); the memory of
session 6's run, which its test does not report; the resident set on Windows or macOS (the figures follow
glibc's allocator, which keeps what a program frees); the via transition on 0.2 mm cells, not run again; which
solver is nearer a finer mesh on the via transition (D90).

### D99. Exchange oracles in CI: FreeCAD from its pinned AppImage, the IDX schema fetched from prostep ivip; the IPC-2581 schema cannot be had (2026-11-17)
D81 left M9 with an open item: FreeCAD on the STEP files and `xmllint` on IPC-2581 and IDX were optional oracles
that no automatic run included. CI has a job `exchange` for them now, and two of the three run in it.
- **A job of its own**, informative (`continue-on-error`) like `oracles`, `openems` and `corpus`. It depends on a
  download from a publisher's server that drops connections, and that should not take the KiCad tests of the
  `oracles` job with it; it needs neither KiCad nor a display, and is done in three minutes.
- **FreeCAD from the project's AppImage, version pinned.** Ubuntu 24.04 has no `freecad` package (`apt-get
  install freecad`: "has no installation candidate"). The job takes
  `FreeCAD_1.1.4-Linux-x86_64-py311.AppImage` from FreeCAD's GitHub release, checks its SHA-256, unpacks it with
  `--appimage-extract` (no FUSE on the runner) and gives the tests `squashfs-root/usr/bin/freecadcmd`
  (`CADLAB_ORACLE_FREECAD`). It runs without a display and without `AppRun`. A PPA or the snap would follow
  their own releases; the AppImage is the same bytes every run. FreeCAD is LGPL and is run as a separate process,
  as every oracle is (D7).
- **The IDX schema is fetched, not vendored.** Its files carry "Copyright (C) prostep ivip Association
  2006-2019, all rights reserved"; the recommendation is free to use and to duplicate "for use in the context of
  creating software" (D37), which is not a licence to publish the schema in an MIT repository. The job downloads
  `PSI5_IDXv4.5_release.zip` from `www.prostep.org` (SHA-256 pinned), takes the schema directory out of the inner
  `PSI5_IDXv4.5_Schema.zip` and points `CADLAB_IDX_XSD` at it. The archive is kept in the job's cache, keyed by
  the checksum: the runner's own copy, published nowhere, so that a run does not depend on the server, which
  refused about every other connection when this was written. The cached path is under `runner.temp`:
  `actions/cache` saves nothing for a path with `..` in it ("Relative pathing '.' and '..' is not allowed", a
  warning in the post step), which is what `${{ github.workspace }}/../...` gives. The `openems` and `corpus`
  jobs' caches are written that way and the repository holds no cache of theirs; not changed here.
- **The IPC-2581 schema is not in the job, and that part of the item stays open.** `https://webstds.ipc.org/2581/`
  and `IPC-2581C.xsd` under it answer 403 from a GitHub runner, from the development machine and from a third
  network (2026-11-17); the IPC-2581 Consortium's validation page gives that address as where the official schema
  files are. With no access there is no file, no checksum to pin and no terms to read. Copies on other sites were
  not used: whether IPC allows them is unknown, and CI must not rest on one. Nothing in the repository records
  that cadlab's IPC-2581 was ever validated against the schema; the roadmap says so now.
- **A listed oracle cannot skip.** `oracle::optional` and the schema variables skipped quietly when the tool or
  file was missing, which is right on a developer's machine and wrong in a job that installs them.
  `CADLAB_ORACLES_REQUIRED` lists variables (`CADLAB_ORACLE_FREECAD,CADLAB_ORACLE_XMLLINT,CADLAB_IDX_XSD` in the
  job); a test whose variable is listed panics where it would skip. A schema variable that points at nothing
  panics always. The tests write an `oracle:` line when the other program has done its reading, and the job
  checks the log for those lines and for a `skipping:` line naming one of the three.
- **One more check:** `step_packages_freecad_oracle` gives FreeCAD the board with one part per generated package
  family (MELF, SOIC, LQFP, QFN, DIP, BGA, SOT-223, SOD-123, SMA, DPAK), which only cadlab's Part 21 reader had
  read.
Runs (2026-11-16, the job alone on a branch `ci/exchange-oracles`; the first with every step passing is
https://github.com/KarpelesLab/cadlab/actions/runs/43932259429, the job as committed is
https://github.com/KarpelesLab/cadlab/actions/runs/43934428784): FreeCAD 1.1.4 read 24, 297 and 11 valid closed solids on the three boards, the board volumes equal to
the ones computed from the outline; the three IDX messages are valid. No defect in the outputs.
*Why:* an exit that says "leaves cadlab" should rest on a program other than cadlab reading the file (D81), and
a check that can skip without saying so in the place meant to run it is not a check.
*Not done:* IPC-2581 by any other program; an IDX reader (none is available, D37); FreeCAD on IDF.


### D100. `sim.sparams` runs on ems alone: ems and csx in every build, the cargo feature `ems` and the built-in FDTD engine removed, `solver` no longer a parameter (2026-11-17)
D98 measured what D94 had left open and ended on a decision that was not a measurement: the built-in engine was
the only solver of a build without the cargo feature `ems`, and removing it meant one of three things. The
first is taken: **ems is always there.** `ems = "0.0.12"` (with csx 0.0.3 through it, as `ems::csx`; both MIT,
from crates.io, no further dependencies, MSRV 1.95 as cadlab's) is a dependency of every build, the feature and
every `cfg(feature = "ems")` are gone, and the engine is removed. No openEMS, CSXCAD or other GPL code was read;
nothing was changed in ems.
**Removed** (5500 lines deleted, 4800 net; 3400 of the deleted in `src`, the rest tests): `fdtd::engine` (2253 lines: the Yee update on
the graded mesh, Debye poles, the surface impedance on conductor loops, CPML, lumped elements, probes, its
running DFT and its thread pool), `fdtd::conformal` (575: its Dey–Mittra cells), the surface-impedance fit of
`fdtd::fit` (`surface_fit`, `surface_impedance`; ems fits its own), the voxelizer and the engine's run in
`fdtd` (`build`, `prepare`, the second half of `sparams`), `parts::place` (elements on voxelized copper), the
`Solver` enum, `Options::solver`, and the error `sim.solver_unavailable`.
**Kept**, being what the model is built from whoever solves it: `fdtd::extract` (the region, the mesh refined
for narrow diagonal tracks, the cell guard, the candidates for lumped parts), `place_ports`, `Waves` (S = B A⁻¹
and the Touchstone result), `notes`, `planned`, the Debye fit of `fdtd::fit` (the openEMS export writes the same
terms), `parts::candidates` and `parts::boxes`, all of `openems`, `touchstone`. What the adapter took from the
engine moved to `fdtd`: `Gaussian` (the pulse's length, for the plan), `Hooks`, `LumpedPort`, `Probe`; `EPS0`
and `C0` are `openems`'s. Of `fdtd::conformal` one function is left, `narrow` (which slanted tracks are too
narrow for the mesh), as `fdtd::slanted`. The library's public `fdtd::engine` and `fdtd::conformal` are gone
with no replacement: a library user who drove the engine directly uses ems.
**`solver`.** The parameter is removed from `sim.sparams` outright: with one solver it chose nothing, and a
value kept only to be refused (`builtin`) or to be ignored (`ems`) would be a parameter that lies. The command's
arguments are `deny_unknown_fields`, so a request that still names `solver`, with either value, gets
`args.invalid` naming the field and listing the arguments there are, as any unknown argument does. The *record*
stays and says more: the output's `solver` and the Touchstone file's first comment line are `ems 0.0.12`, the
crate and the version this build depends on (`fdtd::SOLVER`; `Cargo.toml` asks for exactly that version, a
0.0.x requirement, and a unit test holds the constant to the manifest). The text result says "with ems 0.0.12:".
**Old inputs.** Nothing saved holds the parameter: `sim.sparams` is a query, the history stores project
snapshots and not requests, and no project file, golden file or snapshot names `solver` or `builtin` (checked
by grep; `tests/snapshots/cli__commands.snap` lists the command by its summary, unchanged). The output type's
`solver` is now a string with a default, so a stored result with `"solver": "builtin"` or without the field
still deserializes, and its text names what it holds. A Touchstone file is read by `touchstone` whatever its
comments say; one that says `(solver: builtin)`, or names no solver (before D94), is the removed engine's. The
MCP tool schema and the CLI flags are generated from the command, so `--solver` and the `solver` property are
gone from both; the command's input schema in `describe` is 10 644 bytes of the 11 000 its test allows.
**Tests.** Every board-level test that ran on both solvers runs on ems alone, at the tolerance it had
(`sim_sparams_command`, `sim_sparams_models_parts`, `cut_cells_do_not_change_a_parts_columns`,
`sim_sparams_conformal_option`, `narrow_diagonal_track_refines_the_mesh`, and the `#[ignore]` validations
`sim_sparams_microstrip_validation`, `lossless_line_near_the_pml_is_passive`, `sim_sparams_mask_validation`,
`sim_sparams_parts_validation`, `routed_45_degree_line`, `rf_path_full_wave`); their references were never the
other solver (the 2D field solver, D52's loss model, circuit theory, passivity, the straight line, the ideal
chain). The openEMS oracle compares openEMS with `sim.sparams` once per case where it did so per solver. Where a
test's only reference was the built-in solver's result, the reference is now a recorded figure:
- `ems_runs_the_region` held ems to the engine on a coarse lossy microstrip (|S21| within 0.01 and |S11| within
  0.015 between microstrip ports, |S21| within 0.05 between lumped ports) and asserted the two outputs' mesh and
  ports equal. The engine's |S11| and |S21| at the eleven frequencies of both runs are constants in the test
  (`RECORDED_MSL_S11`, `RECORDED_MSL_S21`, `RECORDED_LUMPED_S21`), taken from `sim.sparams` with
  `solver: builtin` at commit 3aa423f, the last with the engine, in a debug build on Linux x86-64, to four
  digits; the test also holds the mesh to the line counts the figures were taken on. ems reads within 0.0020,
  0.0034 and 0.0026 of them; the tolerances are 0.01, 0.015 and 0.01 (the lumped one was 0.05, set when the two
  read 0.030 apart at 9 GHz). The equality of mesh and ports is dropped: both came from the same code.
- The engine's unit-level fixtures had ems counterparts that only printed (`measure_lumped_fixture`,
  `measure_cavities`: ems directly on csx models of the same strip-in-a-box and the same cavities). They assert
  now, at the tolerances the engine was held to: `lumped_fixture_matches_circuit_theory` (not ignored; series
  R within 0.003 in |S21| and 0.015 in |S11|, reactive series elements 0.02, the shunt capacitor 0.02, the
  notch 0.03, and a shunt 50 Ω, new, 0.003) and `cavities_match_their_modes` (`#[ignore]`; PEC resonances
  0.4 %, Q from σ 3 %, the plates' Q 1 %, the Debye filling's resonance 0.3 % and Q 1 %). Measured with 0.0.12:
  0.0009 / 0.011, 0.016, 0.007, 0.009, 0.0019; −0.05 and −0.08 %, +0.04 %, +0.08 / −0.14 / −0.22 %, −0.01 %,
  −0.74 / −0.65 / −0.67 %. References: circuit theory and the analytic modes.
Dropped, because they compared the two solvers or measured the engine and nothing else: the engine-level tests
of `tests/fdtd.rs` (its microstrip probes against the field solver, cavities, energy conservation, the Courant
limit, its thread independence, its surface impedance and Debye terms against D52), of `tests/fdtd_conformal.rs`
(its cut cells on diagonal strips, cylinders, a round patch, a strip resonator; energy on cut cells) and of
`tests/fdtd_parts.rs` (its elements on the box fixture), the unit tests of `fdtd::conformal` and of the surface
fit, `measure_lossy_microstrip` and `measure_via` (two solvers side by side; the openEMS oracle holds both cases
to an outside reference), `rf_path_full_wave_ems` (folded into `rf_path_full_wave`, which takes
`CADLAB_S6_THREADS` and prints the time), and `CADLAB_MEM_SOLVER` of `measure_memory`. The other `measure_*`
tests print ems alone. No golden file and no snapshot changed.
**What is lost.**
- *The second result on the same model.* Two solvers in one process, on the same mesh and timestep, answered
  "which of the two is off" without installing anything: ems's extra conductor loss on diagonal copper (D87,
  D89), its two points of laminate loss on a long line and its 1.4 to 1.7 times the steps to a deep end
  criterion (D89, D90) were found that way. What
  is left is the openEMS oracle (CI only), the recorded line above, the result's own passivity and reciprocity
  checks, and a second mesh. A regression in ems that keeps a result passive and inside the validations'
  tolerances would now be seen only against openEMS, at that job's tolerances (15 % + 0.025 dB on the lines,
  0.25 dB on the via).
- *A build without the solver dependency.* `--no-default-features` no longer drops ems and csx: 21 s of a
  release build, and two more crates for a library user who wants the board model only. There is no build of
  cadlab without a full-wave solver, and none where `sim.sparams` is refused for lack of one.
- *Memory.* ems takes 1.3 to 1.8 times what the engine took (D98: 91 to 97 MB against 57 MB on 580 000 cells,
  58 to 62 MB against 34 to 35 MB on the via transition), and by ems's account that is not about to change.
  The lower figure is no longer available by choosing a solver.
- *The engine's own measurements* in ELECTRICAL.md (conformal cells on cylinders and patches, the surface
  impedance on a strip resonator) are in the history, not in the document, which describes ems.
*Why now:* D98's two measurable reasons to keep the engine were gone (one thread within 1.1 times, session 6
faster at equal threads), the oracle job had run twice with ems as the default, and a second engine kept only
as a reference costs its 2800 lines, their tests in every CI run and a `solver` axis in every board-level
test, for a comparison whose results have not moved since ems 0.0.10 (D94, D98: the same bytes).
**Verification** (Linux x86-64, six threads on a shared machine, ems 0.0.12 from crates.io). `cargo fmt --all
--check`; `cargo clippy --all-targets --all-features -- -D warnings`, and the library with
`--no-default-features` (`--all-targets --no-default-features` does not build, before and after this change:
`tests/render_board3d.rs` needs `png`); `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`;
`cargo +1.95 check` with all features and with none; the test targets `fdtd`, `fdtd_ems`, `fdtd_parts`,
`fdtd_conformal`, `openems`, `agent_session6`, `agent_ergonomics`, `cli`, `mcp` and the `fdtd` unit tests in a
debug build; and in release every `#[ignore]` validation named above: `sim_sparams_microstrip_validation`
(|S21| −1.3 to +5.3 % of D52, Z₀ +1.0 to +2.9 %, εeff +0.7 to +1.0 %), `lossless_line_near_the_pml_is_passive`
(0.9991 to 1.0007), `sim_sparams_mask_validation` (Z₀ +1.1 %, εeff −0.3 %), `sim_sparams_parts_validation`
(0.007, 0.009, 0.008), `routed_45_degree_line` (164.2° straight; 154.7° and 157.7° bent with conformal cells,
104.1° and 143.5° as staircases; the 0.3 mm track on 2.46 million cells 141.1°, 156 s),
`cavities_match_their_modes`, and `rf_path_full_wave` (|S11| 0.5726, |S21| 0.7988, D98's figures; 14 529 steps
per port, 33.9 s). All pass. The full `cargo test --all-features` was not run here (the merger's), `cargo deny`
is not installed here and openEMS is not either: those are CI's, below.
CI on the branch `ci/ems-always` (https://github.com/KarpelesLab/cadlab/actions/runs/43979594388, the two
commits of this change before this paragraph): all eleven jobs passed, among them the tests on Linux, Windows and
macOS (the recorded line holds on all three), MSRV 1.95, cargo-deny, and the openEMS oracle, nine tests: the
lossy lines within 36.0 % (sheets) and 40.2 % (`solid`) of `sim.sparams` at the lowest frequencies, inside the
test's absolute term, Z₀ within 4.3 and 1.9 %, the via transition within 0.07 dB.
*Not established:* what ems's memory or time is on Windows or macOS (D98); times on an unshared machine with
more cores; whether a future ems release keeps the recorded line (the test will say); the via transition on
0.2 mm cells, not run again (D90).

### D101. guide 0.0.10 and 0.0.11 adopted: its passes of rip-up end where they have stopped gaining and what they leave open is settled by chains kept only if the board has less open; the rows of fine pitch planned on boards of two layers; the default unchanged (2026-11-17)
guide 0.0.10 is for the third of D97's gaps, rip-up that settles, with a side on the second, the pours that routes
shred (guide's `docs/benchmarks.md`, "PCB routing settled"). `guide = { version = "0.0.10", default-features =
false, features = ["pcb"] }`; nothing else in `Cargo.lock` moves.
**What guide 0.0.10 does** in a run with a budget or a deadline (`RouteOptions::settle`, its default):
- *A pass ends where it has stopped gaining*: once every net has had a turn since its routes made the most
  connections they have made and 2 million nodes have gone since (in the first pass, as many again as it took to
  get there, if that is more), and goes back to those routes. corne-cherry's first pass has 395 of 432 connections
  after 4 million nodes, and 0.0.9 had 396 at best 23 million nodes and some hundreds of rip-ups later.
- *Chains*: each connection still open is looked for a route across other nets' routes, four nets at most; the
  nets crossed are cut where it crosses them and joined up again at once, each across further nets if it finds no
  way round, three nets deep and twelve in all. If every cut net is whole again and the board has less open, the
  chain is kept; if not, every net has the copper back that it had, and the next try, of four, does not cross the
  net that was not joined up again. No chain leaves less than it found.
- *What a chain is judged by is what cadlab will count*: the connections open and the pieces of fill with pads on
  them that the routes cut off the fill their net is planned on (the grid of `pour_cuts`, D97), and a net with
  such pieces has chains of its own first. Without that a chain made a connection through corne-cherry's ground
  pour and left two pads of the ground behind its track: the run said 23 open where cadlab found 40.
**What the adapter takes from it** (`router::guide`; ROUTER.md, "guide as a backend"):
- *`settle` on*, guide's default, set by name; `Phase::Settle` is told as "settling what is open".
- *`row_fanout` on a board of two copper layers*, off on any other (`ROW_FANOUT_LAYERS`). D97 left it off: a gain
  on corne-cherry and nrfmicro and a loss on glasgow-revC3 and buspirate5-rev10. With 0.0.10 the same split, by
  less, and it is by layers: at 40 million nodes over three orders nrfmicro 20 to 11 and corne-cherry 140 to 130,
  and on four layers buspirate5-rev10 11 to 15, tinytapeout-demo 38 to 43, glasgow-revC3 107 to 109, cynthion the
  same. A pad of a fine row on two layers has no inner layer to drop a via to, and the vias of its row are on the
  layers that every route is on; on four layers a row's vias are at the ends of its pads and the plan is in their
  way. The cost: on the generated stm32-2l, which completes either way, 126 vias for 72 and 1300 mm for 1146 mm.
**Measured** (ROUTER.md, "Native router and guide 0.0.10, D101"). The machine had 68 to 75 runnable threads on its
64 from other work through all of it, where D97's hour had 5 to 9, and a minute of one thread made about half the
nodes: the native router has 370 open on the twelve boards for D97's 294, and guide 0.0.9 behind D97's adapter 419,
468 and 362 for 273, 250 and 238. So there are two measures.
- *A budget of work*, 40 million of guide's nodes, the same run under any load, one run of guide, three orders: the
  nine boards that a budget cuts have 853 open with 0.0.10's source for 892 with 0.0.9 (corne-cherry 140 for 169,
  cynthion 63 for 67, glasgow-revC3 107 for 111, buspirate5-rev10 11 for 13, the others the same; no board more in
  sum), and with the rows on the boards of two layers nrfmicro 11 for 20 and corne-cherry 130.
- *60 s in this hour*, three orders, the three builds by turns in one queue: 0.0.9 419, 468 and 362; 0.0.10 behind
  the same adapter 419, 425 and 354; after 412, 408 and 343. The eleven without glasgow-revD1: 177, 149 and 166;
  177, 153 and 163; 170, 143 and 156. nrfmicro 4, 7 and 9 to 3, 5 and 3; corne-cherry 45, 38 and 43 to 36, 38 and
  40; lumenpnp-mobo 2, 1 and 5 to 4, 8 and 5 (pads off their pours: its first run is complete at 39 to 42 s of
  its 42 s in this hour, and where it is not back in time the second run is not started; 0, 0 and 1 for 0, 1 and
  0 with a budget of work). Zero DRC errors other than unrouted on every run, no via in a pad of its net, nothing
  taken out by the verification; the longest call 61.8 s (61.4 s with 0.0.9 in the same hour, 58.8 s in D97's).
**The generated boards** through the adapter of this change with 0.0.10: one run each with the bench's 120 s, beside 0.0.9 behind D97's adapter in the same hour. Seven of eight complete with no DRC error with both (attiny-2l 0 vias, soic24-2l 159, stm32-4l 51, qfp-qfn-4l 172, fine-4l 105, bga144-4l 248, the same routes); stm32-2l, of two layers and so with its rows planned, is complete with 126 vias and 1300 mm in 5.4 s for 72 vias and 1146 mm in 2.4 s; and big-4l, which the load decides: 59 of its 1275 connections open at 118.8 s, cut, in a first run while a build was going on beside it, and 3 open in 95 s in a second, with the routes of 0.0.9 run beside it to the via (2268 vias, 21 592 mm; 0.0.9 alone 4 open in 96.7 s; complete in 70 s in D97's hour). What guide 0.0.10 does cost in time, which a budget of work does not count: with every net of a board routed, its chains for the pads cut off the pours go on for seconds (lumenpnp-mobo's first run is in "settling what is open" from 25.2 s to 39.0 s with 193 of 193 nets routed, and its cleanup has 3 s for 7).
**guide 0.0.11**, of the same day, is 0.0.10 without its chains for the pads cut off their pours. 0.0.10 gave a net
with pieces of pour cut off chains of its own before the others; they made no more (at 40 million nodes over three
orders, six boards have 232 open without them for 235), and with every net of lumenpnp-mobo routed they went on
from 25.2 s to 39.0 s of a first run that had 42 s on the loaded machine, a ground of two hundred pads searched
for at four times a search's limit, so that the run was back too late for the second, which is what joins the pads
off its pours: the 4, 8 and 5 for 2, 1 and 5 above. The pieces are still counted in what a chain is judged by, and
are joined by guide's stubs after routing and by cadlab's second run. `guide = { version = "0.0.11", … }`; nothing
changes in the adapter.
**Measured again, with 0.0.11, on the machine as D97 had it** (ROUTER.md, "Native router and guide 0.0.11, D101";
a load of 4 to 10; the native router 290 for D97's 294, guide 0.0.9 273, 249 and 243 for 273, 250 and 238). Open
on the twelve boards over the three orders: 0.0.9 behind D97's adapter 273, 249 and 243; 0.0.10 behind the adapter
of this change 267, 249 and 236; 0.0.11 behind it 263, 249 and 234; the native router 290. The eleven without
glasgow-revD1: 111, 113 and 119; 105, 113 and 112; 101, 113 and 110; native 90. nrfmicro 4, 7 and 9 to 3, 5 and 3,
back after 6 to 10 s for 23 to 28; corne-cherry 36, 35 and 38 to 31, 37 and 36, with 472 vias and 6084 mm for 825
and 8436 mm; buspirate5-rev10 2, 6 and 4 to 0, 6 and 3; cynthion 19, 14 and 23 to 17, 14 and 23 with 576 vias for
657; glasgow-revC3 one more in one order, no other board more in sum. Zero DRC errors other than unrouted, no via
in a pad of its net, nothing taken out by the verification, no call over 58.1 s. This is the table that the
default is judged by; the one of the loaded hour is kept as what a loaded machine does to it.
**The generated boards** with 0.0.11: one run each with the bench's 120 s, beside 0.0.9 behind D97's adapter at the same time: all eight complete with no DRC error with both, and seven with the same routes (attiny-2l 0 vias in 0.02 s, soic24-2l 159 in 1.1 s, stm32-4l 51 in 1.1 s, qfp-qfn-4l 172 in 4.7 s, fine-4l 105 in 3.5 s, bga144-4l 248 in 15.7 s, big-4l 1994 vias and 21 789 mm in 77 s for 79 s). stm32-2l, of two layers and so with its rows planned, has 126 vias and 1300 mm in 3.2 s for 72 vias and 1146 mm in 1.5 s: what the rows cost a board that completes without them.
**Tried and not kept**, in guide, each at 40 million nodes (its `docs/benchmarks.md`):
- *Chains kept with a net left cut*, where the routes made more in all: corne-cherry's chains went on to thirteen
  nets with more open at their end than at their start, 122 open over three orders at 60 s for 120 without.
- *Chains half as long and half as deep at three times the price of a crossing*: corne-cherry 144 over three
  orders for 135.
- *Windows*: after the chains, the nets of few pads with a pad within eight pitches of the open pad taken out
  whole and routed again after it, up to three times with the net that found no way first: corne-cherry 145 for
  140, every other board the same to the connection.
- *A fixed share of the budget for the chains*, seven tenths for the passes, with no end to a pass: corne-cherry
  277 over six orders for 274, nrfmicro 29 for 31, in three times the seconds.
- *`row_fanout` on every board* (above).
And not built, after looking: *sweep-v2.2 as two boards*. Its 6 to 8 open against 3 are its ground, `raw` and
`vcc`, each with a search that floods the board to its limit of nodes. The board is two halves in one outline,
joined by a strip beside a slot at its middle, 217 mm wide: one region, so there is nothing to split the nets by,
and what the three nets lack is a search over a wide board that ends, D97's item 4 as it was.
**Gaps, for guide's next release**, most useful first:
1. *A way out of the pads of a fine row on two layers that the neighbours leave open*, D97's item 1, as it was:
   corne-cherry has 31, 37 and 36 against 3. Thirty are pads of its two QFN-56 and its USB receptacles with the
   tracks and vias of four other nets round them: a chain for such a pad is five nets long before it starts, and
   seven chains in eight are undone. The rows planned as routes help by ten connections in three orders; what
   the native router has is the whole package's exits chosen together and kept through negotiation, where every
   net gives way a little, and guide's rip-up moves one net at a time.
2. *Pours on outer layers*, D97's item 2, less than it was: the chains count the pieces cut off and join some, and
   corne-cherry still has 5 to 13 pads of GND and GNDR open in an order.
3. *nrfmicro's last connections*: 3, 5 and 3 against 0, back after 10 to 20 s with the rest of the minute unspent.
   The pads of its USB receptacle's D+ and D−, which the native router gives a crossover via, and a pin of the
   module with four nets round it.
4. *Searches on a wide board* (sweep-v2.2, above).
5. *The time a loaded machine leaves*: the first run of lumenpnp-mobo ends at its deadline in this hour and the
   second is not started. A run that knew it had made every connection could come back before its cleanup.
6. *The rows where they are not needed*: the generated stm32-2l completes without them with 72 vias and with them
   with 126. The adapter's rule is by layers, and a rule by what a row needs is guide's to find.
7. *Rule areas by kind and containment with a routed track width*, and *vias*, D97's items 5 and 6, not touched.
**Default: unchanged.** The condition is that guide be at least as good as the native router on every board. With
0.0.11 on the machine as D97 had it, three orders against the native router's one run: level on three
(buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight); ahead in every order on three (glasgow-revD1 162, 136
and 124 against 200; lumenpnp-mobo 0, 1 and 0 against 15; cynthion 17, 14 and 23 against 27); within what runs
differ by on three (tinytapeout-demo 9, 6 and 5 against 6; buspirate5-rev10 0, 6 and 3 against 2; glasgow-revC3
35, 37 and 34 against 34); behind in every order on three (nrfmicro 3, 5 and 3 against 0; sweep-v2.2 6, 7 and 6
against 3; corne-cherry 31, 37 and 36 against 3). 263, 249 and 234 against 290 on the twelve boards, ahead in every
order; 101, 113 and 110 against 90 on the eleven without glasgow-revD1, behind by 11, 23 and 20 where D97 had 18,
21 and 21, and corne-cherry alone is behind by 28, 34 and 33. In the loaded hour, with 0.0.10: 412, 408 and 343
against 370, behind in two orders of three, and behind in every order on buspirate5-rev10 too (9, 8 and 10 against
2): a loaded machine costs one thread of guide more than three of the native router. `router: guide` stays the
second opinion.
*Why:* the chains are the smallest change that makes rip-up monotone: a pass could always end below its best, and
its order of coming back was nobody's. They gain where routes block each other loosely and not where a pad is
walled in by four nets, which is most of what is left. The rows by layers are a host's rule on a measured split,
not a property guide knows of a board.
*Not measured:* the native router over three seeds; the table of the quiet hour a second time; 60 s with the rows on the boards of four layers; the work-budget table with the released
crate (it is of the release's source, before the other changes of 0.0.10, which are not in `pcb`); budgets other
than 60 s and 40 million nodes; cadlab's oracles; KiCad's DRC on a board guide routed.


### D102. freerouting on the open-source corpus: 2.5.0 run on the exported DSN and counted by cadlab's DRC beside the native router and guide; pours as planes in `export.dsn`; a CI job for the table; the corpus cache path (2026-11-17)
M6's exit asks for equal or better completion than freerouting, and D84 recorded it as met on the generated boards
only: freerouting had never been run on the twelve corpus boards. It has now, and the numbers are in ROUTER.md
("freerouting on the corpus"). freerouting is GPL-3.0: its released jar was run as a separate process, its
command-line and settings documentation was read (`docs/command_line_arguments.md` and `docs/settings.md` of its
repository at `v2.5.0`), and no source of it was read, downloaded or ported (D7).
- **Where the measurement lives: the corpus mode of `route_bench`, not a test.** `CADLAB_BENCH_ROUTER=freerouting`
  takes the place of the native router and guide in the one loop that imports each board, removes its routing and
  counts the result, so open connections (`drc.unrouted` with the pours filled again), DRC errors, vias, track
  length and wall time come from the same lines for all three. A test would have to assert something, and a
  routing result under a time limit is not a thing to assert; what must not happen is a board silently missing
  from the table, so the run exits with an error when freerouting writes no session for a board or the session
  cannot be applied.
- **freerouting 2.5.0, pinned by version and SHA-256** (`f6f51bb0…5de3c7`, the jar of the GitHub release). It
  needs Java 25, which the jar says by refusing to load on 21; `CADLAB_ORACLE_JAVA` names the `java` to use
  (`JAVA_HOME` and the `PATH` after it), here and in the two older places that start it. 2.4.1, which the
  generated boards' table has, is not run again; the same boards with 2.5.0 are a second table.
- **Its settings are its defaults, with one limit: time.** Fanout, routing and optimizer stages on, no limit of
  passes, `--router.job_timeout` as the budget. A fixed `-mp` would have been a choice of ours about how long it
  may try; its own limit lets it use the time as it sees fit and still write a session. The fanout stage costs it
  the minute on the larger boards, and turning it off was tried on one board and was worse. `-da` (no usage data),
  a data directory of the run's own (the developer's `freerouting.json` is not read), `-XX:ActiveProcessorCount`
  and `-mt` to the thread count allowed (1 here, 4 in CI; its routing passes are single-threaded by its own log).
- **Two budgets, so that it is neither cut short nor given an hour against a minute unremarked.** 60 s, the
  budget every corpus table since D88 has; and 30 minutes, with 2 hours on three boards. The native router was
  given the same 30 minutes on the boards its 60 s cut.
- **`export.dsn {planes: true}`** writes each pour that has a net as a Specctra `plane` (its outline on each of its
  layers). Without it the comparison had a hole anyone would point at: a router that is not told of the ground
  pour must wire the ground. The default stays without planes, and the measurement says why: freerouting counts a
  pad under a plane's outline as connected, cadlab's fill does not always reach it, and on these boards the result
  with planes has more connections open by cadlab's count on nine of twelve boards and fewer on one. Teardrop zones are written as
  planes like any pour; leaving them out is not done (a zone is a zone to the model).
- **DRC errors are split by whether the DSN held the rule.** freerouting cannot be held to a minimum track width,
  a hole or edge clearance, a pad's own clearance, a custom rule or a copper drawing that no DSN construct
  carries, and it cannot be excused for them either: both counts are in every cell. A clearance error is "not
  carried" only when its gap satisfies the clearance the DSN gave the two nets.
- **`tests/specctra.rs` runs with 2.5.0 and allows `drc.track_width`.** 2.5.0 lays two tracks of the STM32 board
  at 0.125 mm where the board's minimum is 0.15 mm. The test's purpose is the geometry and units of the exchange,
  and the DSN has no minimum to give; every other error still fails it, and unrouted connections too.
- **CI job `freerouting`,** informative (`continue-on-error`, 150 minutes at most, 46 as measured): Temurin 25,
  the jar downloaded with its checksum checked and cached by it, `freerouting_oracle` (no other job has Java; the
  log must show both boards routed), then the four 60 s tables on the runner. Nothing of freerouting is in the
  repository or in the artifact, which has the tables and logs. The 30-minute runs are not in CI: twelve boards
  at 30 minutes are six hours of runner for a table that does not change the 60 s one.
- **The corpus cache path.** `corpus` and `corpus-route` cached `${{ github.workspace }}/../cadlab-corpus`;
  `actions/cache` refuses a path with `..` and saved nothing (D99 noted it and left it). The three jobs that read
  the corpus now use `${{ runner.temp }}/cadlab-corpus` with one key. Shown on the branch `ci/freerouting-corpus`:
  the first run (https://github.com/KarpelesLab/cadlab/actions/runs/44003908234) has "Cache not found" and
  "Cache saved with key: corpus-de1dc1aa…", the second (https://github.com/KarpelesLab/cadlab/actions/runs/44010687386)
  "Cache restored" and "corpus cache hit: true" in all three jobs, every project "present" without a fetch. The
  `openems` job's path is left as it is (another change's).
**Result** (ROUTER.md has the tables). At 60 s on this machine freerouting leaves 1647 connections open on the
twelve boards, the native router 283 and guide 0.0.9 273 (CI runner: 1702, 309, 318); it is level on the three
small boards and behind on nine. With 30 minutes it leaves 1187, and its own count falls for a few routing
passes at most: 2 hours on cynthion and glasgow-revC3 end where 30 minutes did (189 and 132 for 181 and 134).
glasgow-revD1, whose 30 minutes ended in the fanout stage with 557 open, has 309 after 2 hours. The native router with 30 minutes on one thread leaves 1, 24, 30, 6 and 43 on the five boards
its 60 s cut. freerouting is ahead in one place in completion, lumenpnp-mobo given time (0 open after 5.5 minutes
for the native router's 10 at 60 s and 1 at 30 minutes; guide has 0 in 32 s), and in vias against guide (345 for
713 there). Its routes break rules the DSN does not carry about 1200 times at 60 s (1061 tracks under the minimum
width) and rules it does carry 14 times (13 tracks in nrfmicro's outline notch, a via on a ring pad).
*Why it matters for the roadmap:* the M6 exit is met at 60 s on the whole corpus and, against freerouting given
30 minutes, on eleven boards; lumenpnp-mobo is the board that keeps the line from being closed without a
footnote, and the roadmap has it as an item.
**Verification** (Linux x86-64, at most four threads, shared machine). `cargo fmt --all --check`; `cargo clippy
--all-targets --all-features -- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`;
`cargo test --test specctra` (4 tests; `freerouting_oracle` with `CADLAB_ORACLES=1` and the 2.5.0 jar: both boards
routed at the first attempt, 154 s). The full `cargo test --all-features` was not run here; CI ran it on the
branch: all twelve jobs of the second run above pass (tests on Linux, Windows and macOS, MSRV 1.95, cargo-deny,
the oracle jobs), and all twelve of a third run on the commit before this text
(https://github.com/KarpelesLab/cadlab/actions/runs/44031677107; its four sums are 303, 328, 1689 and 2935);
after it only comments in `route_bench.rs` and `ci.yml` and the documents changed, and no run was made of that.
*Not established:* the spread between runs of freerouting (one run per cell; two loads and three CI runs are all
there is); freerouting with more processors than 4; other settings of it than fanout off (one board) and
neck-down off; whether the via on glasgow-revD1's ring pad is its reading of the slit polygon; a cache hit on
`master` (caches are per branch: the first run after the merge saves it).

### D103. ems beside openEMS, measured: time, memory and accuracy against references that are neither solver; ems 0.0.13 and 0.0.14 (2026-11-17)
D100 left `sim.sparams` on ems alone, and the openEMS oracle job as the one place where a second solver reads
the same model. That job said one thing, that the two agree (the lossy lines within 36.0 and 40.2 % at the
lowest frequencies, inside the test's absolute term; the via within 0.07 dB). It did not say which is faster,
which takes less memory, or which is nearer the truth where they differ. This change makes it say so, and then
closes in ems what the measurement showed. No openEMS, CSXCAD or other GPL code was read: openEMS is run as a
program, and what is read of it is its console output and its result files.
**The measurement** (`solver_comparison`, `tests/openems.rs`, `#[ignore]`; the `openems` job runs it after the
oracle tests). The oracle's five cases, on the options the oracle tests use: the lossy line with sheet copper
between microstrip ports (`line_msl`) and with `solid` copper between lumped ports (`line_lumped`), the via
transition (`via`), a series 50 Ω and a series 1 pF on a microstrip (`series_r`, `series_c`). Per case and
solver, on the same runner: the whole run's time, the time stepping's share of it, steps, time step, cells, the
process's peak resident memory and what it was before the run. Every run is a process of its own, one at a time:
a process's peak is the largest run it ever made, and a time taken beside another test's run is not the
solver's. ems runs through `comparison_ems_run`, the test binary started again with the case in the environment
(`sim.sparams` with `threads`; the time stepping from the command's progress reports, a port's first to its
last); openEMS through the exported driver under a wrapper that imports what the driver imports, notes the peak,
runs the driver and notes it again (the engine's time, steps and cells from the lines openEMS prints at the end
of a run). Threads are stated: one, and the runner's count (four logical CPUs), for both; the driver takes
`CADLAB_OPENEMS_THREADS` for it (`numThreads` of `Run`; without the variable openEMS chooses, as before, and the
golden driver has the two lines). Accuracy is each solver's distance from a reference that is neither:
- *the lines:* the loss without the mismatch, −10 log₁₀(|S21|² / (1 − |S11|²)), against D52 on the length
  between the reference planes (the 2D field solver's matrices of the 35 µm strip, Djordjevic–Sarkar, the skin
  effect: `electrical::loss::at`, no FDTD in it), and Z₀ from the ABCD matrix against the field solver's;
- *the parts:* |S21| against circuit theory, twice: the ideal 2 Z₀ / (2 Z₀ + Z) the oracle test uses, and the
  part between its lines as the field solver has them (50.8 Ω and εeff 3.07 for 5.15 mm on each side, in 50 Ω);
- *the via:* there is none. The two are held to each other, and to each solver's run on a finer mesh where the
  job is asked for one (`CADLAB_CMP_FINE`, the workflow's `openems_fine` input).
Nothing is asserted there but that every run ends and converges: the tolerances are the oracle tests'. The table
is the job's summary and `openems_comparison/comparison.md` in its artifact, written after every case, with
openEMS's console output beside it.
**The job's cache.** `OPENEMS_PREFIX` was `${{ github.workspace }}/../openEMS`, and `actions/cache` saves
nothing for a path with `..` in it (D99 saw the warning): openEMS was built on every run, three minutes. The
prefix is `$RUNNER_TEMP/openEMS` now (set in a step: the `runner` context is not there for a job's `env`), the
key has a new suffix. Run 44002157976 built and saved it ("Cache saved with key: openems-…-ubuntu-24.04-temp",
71 MB); run 44006837399 restored it and skipped the build, and so did the runs after it. The `corpus` job's
cache has the same fault and is another change's.
**What it found, with ems 0.0.12** (run 44013970406, an AMD EPYC 9V45 runner with four logical CPUs; runs
44002157976 and 44006837399 on an Intel Xeon 6975P-C read the same results and other times: a runner is one of
several machines, so two runs compare by their ratios, not by their seconds):

| Case | Whole run on one thread, ems / openEMS | Each at its fastest (threads) | Peak memory | Of it the run |
|---|---|---|---|---|
| `line_msl`, 67 200 cells | 14.7 / 21.1 s (1.4×) | 7.7 (4) / 21.1 (1) s (2.8×) | 46 / 75 MB | 22 / 24 MB |
| `line_lumped`, 85 320 | 27.3 / 65.7 s (2.4×) | 14.4 (4) / 65.7 (1) s (4.6×) | 51 / 78 MB | 28 / 27 MB |
| `via`, 130 626 | 101.2 / 204.4 s (2.0×) | 56.8 (4) / 204.4 (1) s (3.6×) | 65 / 109 MB | 42 / 58 MB |
| `series_r`, 69 312 | 7.9 / 12.6 s (1.6×) | 4.7 (4) / 12.6 (1) s (2.7×) | 37 / 71 MB | 13 / 20 MB |
| `series_c`, 69 312 | 7.5 / 13.0 s (1.7×) | 4.7 (4) / 13.0 (1) s (2.8×) | 36 / 71 MB | 13 / 20 MB |
| `via_same`, 130 626 | 99.9 / 150.3 s (1.5×) | 59.3 (4) / 150.3 (1) s (2.5×) | 65 / 91 MB | 42 / 40 MB |

(On the Xeon runner 1.8 to 2.9 times on one thread and 2.2 to 5.7 times at each solver's fastest.) openEMS's
time is its engine's but for a second or two (its setup is 0.6 to 2.0 s of a run); ems's setup was 1.3 to 3.1 s,
a tenth to a sixth of its short runs. On this runner openEMS on four threads is slower than on one on every case
(four logical CPUs; on the Xeon it gained on the parts and the via): its fastest is one thread, ems's is four.
The 51 MB of openEMS before a run are Python with numpy and its own modules; ems's 23 MB are the test binary and
the board.
- *ems is faster on every case and thread count*, 1.4 to 2.4 times on one thread: it takes fewer steps on the
  lines and the parts (6145 for 8820, 12 065 for 22 221, 7105 for 8037 and 8460, to the same −50 dB; on the via
  32 961 for 27 216 to 29 197) and, but for the sheet line (62 for 63 million), more cell updates a second (80
  to 157 million for 50 to 106).
- *ems takes less memory as a process on every case.* Of the run itself it takes less on four cases and as much
  on two: the lossy `solid` line (28 for 27 MB) and the via on the same mesh (42 for 40 MB).
- *Loss of the lines, from 0.5 GHz: ems is the nearer one.* 0.019 dB from D52 at the most on the sheet line
  (4.3 % RMS, +2.6 % in the mean) where openEMS is 0.027 dB (15.8 %, +13.1 %); 0.040 dB (5.9 %, +4.6 %) for
  0.086 dB (17.0 %, +15.1 %) on the solid line. **The 36 to 40 % of the oracle job are openEMS's**: at 0.56 GHz
  D52 has 0.0426 and 0.0511 dB, ems 0.0422 and 0.0486 dB, openEMS 0.0576 and 0.0681 dB, a third over. Nothing to
  fix in ems there.
- *Z₀ of the lines:* 0.3 % from the field solver for ems and 4.0 % for openEMS with sheets and microstrip ports;
  5.5 and 5.4 % on the solid line between lumped ports, whose own discontinuity is in both.
- *Where openEMS is the nearer one.* At 0.06 GHz, 1 % of `f_max` and below what the oracle test holds: ems is
  0.008 and 0.009 dB from D52 (and reads a gain of 0.001 dB on the sheet line), openEMS 0.005 and 0.004 dB. ems
  has ended at −50 dB after 6145 steps by then; to −70 dB it reads 0.0063 dB for D52's 0.0072. And the parts
  against the ideal: 0.0029 for the resistor where ems is 0.0048, 0.0089 for the capacitor where ems is 0.0099.
- *The parts between their lines.* The ideal takes the board's lines for 50 Ω. The 1 mm strip is the field
  solver's 50.8 Ω, and a 50 Ω part between 5.15 mm of it on each side reads up to 0.004 above the ideal at
  4.5 GHz, as much as the two solvers differ by. Against that cascade (line, part, line, in 50 Ω: circuit theory
  on the field solver's Z₀ and εeff) ems is within 0.0015 for the resistor where openEMS is 0.0054, and 0.0064
  for the capacitor where openEMS is 0.0058. On a finer mesh (0.25 mm) openEMS moves towards the ideal (0.0029 →
  0.0015) and ems away from it (0.0048 → 0.0071), each within 0.0025 of its own coarse run and the two 0.0067
  apart: the pads' gap and the element's own extent are in neither reference, and which solver a measured 0603
  would agree with is not established.
- *The via.* The oracle's case names no `thirds`, which `sim.sparams` has on by default and `export.openems`
  off: its two solvers have run on two meshes since D74 (130 626 cells; 64 × 53 × 55 lines for openEMS, of which
  16 on x and on y are its absorbing cells beyond the region). The comparison has a sixth case, `via_same`, with
  the rule stated for both: 0.047 dB apart from 0.5 GHz in one run and 0.112 dB in the two after it (openEMS's
  steps, next point; 0.067 dB on the two meshes). Against a finer mesh of each (`max_cell` 0.4 mm, 874 000
  cells; one refinement, not a convergence study) ems on the default mesh is the nearer one both times, in both
  runs: 0.088 dB from its own finer mesh where openEMS is 0.131 and 0.200 dB from it, 0.021 dB from openEMS's
  where openEMS itself is 0.064 and 0.133 dB. The two finer meshes are 0.067 dB apart, so the truth of this via
  is not settled by them. The oracle test's case is left as it is (its tolerance was set on it).
- *openEMS's result is not the same from run to run; ems's is.* openEMS looks at its end criterion when it
  prints a progress line, every four seconds or so of wall time, so the number of steps it runs follows the
  machine's speed. The solid line ran 22 221 steps per port on four runs and, on a slower runner, 22 221 and 27
  265 on one thread and 22 960 and 19 475 on four, with another Touchstone file each time (the oracle's 40.2 %
  read 41.6 % there; D98 had seen that point move and put it on openEMS's run); the via on the same mesh 29 197
  and 28 518 steps or 28 518 twice, and with them the 0.047 or 0.112 dB above. ems looks every 32 steps: its
  files are the same bytes on one and four threads on every case, and its data the same lines on every runner.
**What was closed in ems.** Profiles of those runs (callgrind, one thread, 400 steps per port) put 49 to 66 % of
a short run in one function, the stability estimate of conformal PEC: 300 products of the curl-curl operator
over the whole grid in `f64`, once per port's engine, whatever the number of cut cells (twelve to 918 here).
- **0.0.13** (ems PR 13, wave 28 of its design notes). The estimate is made only where it can tell something.
  The largest eigenvalue it approaches from below is at most the largest of the cells' own (the element bound
  ems already used to keep small cut faces), and that is one pass over the cells: where every cell, cut or not,
  is within the limit and the round has no merged or interpolated face, the round is accepted as the estimate
  would have had it, the report's figure is the bound and says so (`ConformalReport::proven`); a cell is first
  tried by the row sums of its six-by-six matrix. Of the engines of one compiled model only the first checks.
  All five regions pass by the bound. And the CPML's rows on z faces, as long as the layer is thick, run in
  loops of their length. Setup of a run with two ports: 1.8 to 7.4 s → 0.3 to 1.6 s on the development machine;
  instructions of 400 steps per port of the 50 Ω part 37.6 → 10.4·10⁹.
- **0.0.14** (ems PR 14, wave 29). The time steps: an edge of a Debye laminate with two to eight poles in loops
  of its length (a third fewer instructions for the laminate's correction on the via, a seventh of its step),
  and a z-face CPML layer a plane of rows at a time, each row computed on copies of its values (27 → 17
  instructions per value). Per step on one thread, in instructions: the lossy line 30.5 → 24.2·10⁶, the 50 Ω
  part 14.6 → 10.5·10⁶.
Result bits: nothing changes. cadlab's Touchstone data are the same lines as with 0.0.12 on the five oracle
cases, the bent line with lossy sheets and with lossy `solid` copper on both meshes and the small via region,
with 0.0.13 and with 0.0.14, each from crates.io; the first comment line differs, which names the solver's
version. ems's own tests hold the engines of a compiled model to a fresh compile bit for bit, with rounds
accepted and rejected (`conformal_checked_once`), and the bound to the estimate
(`the_cell_bound_over_the_grid_is_above_the_ estimate`).
**After.** The before and after of a release are from one machine, because a runner is not: the same 0.0.12 was
1.4 to 2.4 times openEMS's speed on one CPU and 1.8 to 2.9 on another. On the development machine
(an AMD Threadripper 9970X, quiet for these runs; the three versions in turn, one run each, each from
crates.io), the whole `sim.sparams` run:

| Case | One thread, 0.0.12 / 0.0.13 / 0.0.14 | Four threads | Of it setup, one thread |
|---|---|---|---|
| `line_msl` | 11.4 / 10.0 / 9.1 s | 3.9 / 2.9 / 2.6 s | 1.0 / 0.1 / 0.1 s |
| `line_lumped` | 22.1 / 19.9 / 17.9 s | 6.9 / 5.7 / 5.0 s | 1.3 / 0.2 / 0.2 s |
| `via` | 83.9 / 78.6 / 65.0 s | 28.7 / 26.8 / 24.2 s | 2.4 / 0.7 / 0.7 s |
| `series_r` | 6.2 / 4.7 / 4.2 s | 2.4 / 1.4 / 1.3 s | 1.0 / 0.1 / 0.1 s |
| `series_c` | 6.2 / 4.7 / 4.2 s | 2.5 / 1.4 / 1.3 s | 1.0 / 0.1 / 0.1 s |

0.0.14 takes 0.68 to 0.81 of 0.0.12's time on one thread and 0.54 to 0.84 on four; the process's peak is what it
was on these (33 to 62 MB). The cases of D98's table that are not the oracle's, 0.0.12 → 0.0.14: the small via
region on one thread, the one row where D98 had ems behind cadlab's own engine, 36.8, 39.4 → 29.1, 29.2 s
(three threads 15.6 → 12.6, 13.3 s); the bent line on 0.25 mm cells on three threads 21.7 → 15.3 s with lossy
sheets and 29.2 → 21.4 s with `solid` copper, 91 → 87 and 111 → 107 MB. On the runners, beside openEMS in the
same job: with 0.0.13 (run 44020626931, an Intel Xeon Platinum 8370C) ems is 1.3 to 2.8 times as fast on one
thread and 2.0 to 5.2 times at each solver's fastest (`line_msl` 23.6 for 38.2 s, `line_lumped` 49.1 for
136.1 s, `via` 191.9 for 337.4 s, `series_r` 10.3 for 21.3 s, `series_c` 10.4 for 22.4 s, `via_same` 190.0 for
240.9 s), with the memory and the accuracy rows of the table above but for openEMS's own movement. With 0.0.14
the job ran on the Xeon 6975P-C of the first two runs (run 44030782504 beside run 44006837399), so that these
compare:

| Case | ems on one thread, 0.0.12 → 0.0.14 | ems on four | openEMS, one thread / its fastest (0.0.14's run) | ems's lead on one thread | At each solver's fastest |
|---|---|---|---|---|---|
| `line_msl` | 17.1 → 13.9 s | 9.1 → 6.8 s | 32.0 / 32.0 s | 1.9 → 2.3× | 3.6 → 4.7× |
| `line_lumped` | 34.3 → 29.6 s | 17.4 → 13.7 s | 96.2 / 96.2 s | 2.9 → 3.2× | 5.7 → 7.0× |
| `via` | 135.3 → 118.1 s | 72.5 → 67.0 s | 252.0 / 231.2 s | 1.9 → 2.1× | 3.3 → 3.5× |
| `series_r` | 9.7 → 6.8 s | 5.6 → 3.4 s | 17.1 / 12.9 s | 1.8 → 2.5× | 2.2 → 3.8× |
| `series_c` | 9.7 → 6.9 s | 5.6 → 3.4 s | 17.9 / 13.5 s | 1.9 → 2.6× | 2.3 → 4.0× |
| `via_same` | 118.3 s | 67.1 s | 180.9 / 177.8 s | 1.5× | 2.7× |

(openEMS's own times on one thread are within 4 % of each other in the two runs.) Peak memory with 0.0.14, ems
for openEMS: 45 for 75, 52 for 78, 65 for 109, 35 for 72, 35 for 71 and 66 for 92 MB; of it the run 22 for 24,
29 for 27, 42 for 58, 12 for 20, 12 for 20 and 43 for 41 MB. The accuracy rows are the ones above: ems's data
are the same lines, and of openEMS's the solid line reads as in the first three runs and the via on the same
mesh as in the fourth.
**Tried and not kept.** In ems: the first value of a CPML row on the face z = 0, which has no neighbour below,
taken into the row with a zero in its place (the copy cost more than the odd length: 2.34 for 2.27·10⁹
instructions, 170 for 190 Mcells/s); the CPML's uniform rows called instead of inlined (no difference). In the
comparison: a single figure for "accuracy" (the lowest frequency of the lines and the two references of the
parts point the other way from the rest, so each has its row).
**What ems is not ahead in, after this.**
- *The memory of the run itself* on the lossy `solid` line and on the via on the same mesh: as much as openEMS's
  (28 for 27 MB and 42 for 40 MB with 0.0.12, 29 for 27 and 43 for 41 with 0.0.14, a megabyte either way between
  runs; as a process ems takes two thirds). By ems's own account (a counting allocator in the test binary: 22
  and 35 MB live while running, 3 MB more at the peak of the setup since the estimate is gone) it is state, not
  a peak: a Debye pole is a charge and two constants in `f64` per pole and edge (24 bytes, six poles), the
  rational sheet 450 bytes per sheet edge. Narrowing them changes results; not done.
- *The lowest frequency of a run to −50 dB* (above).
- *The parts against the ideal 50 Ω lines* (above; against the lines as they are, see the row).
- *The time step.* ems takes the Courant limit of the smallest cells at the vacuum speed, times cadlab's 0.95;
  openEMS's criterion on the operator with its materials gives a step 2 to 16 % longer on four of the cases
  (0.1982 for 0.1949 ps, 0.1966 for 0.1689 ps on the parts) and a shorter one on the via (0.0460 for 0.0545 ps
  on the same mesh). The stability figures above read 0.66 to 0.74 for these regions at that step, so the scheme
  would carry one 1.35 to 1.5 times as long. That is the largest thing left in time, and it is not taken here: a
  longer step changes every result, and the faces ems keeps on cut cells depend on it.
**Docs.** ELECTRICAL.md's openEMS tables are read again on ems (they were the removed engine's, with a note),
with D52 beside the lines and the two points above; TESTING.md's openEMS row has the comparison. The roadmap's
full-wave items are unchanged but for the versions.
**Verification.** ems: `scripts/check.sh` (fmt, clippy and the tests with all features and with none, docs,
`cargo +1.95 check`) passed on the development machine before each merge; both release PRs had Test & lint on
Linux, Windows and macOS, Docs and MSRV green, and master's CI was green, before `gh pr merge`; 0.0.13 and
0.0.14 are on crates.io. cadlab (ems 0.0.14 from crates.io, Linux x86-64): `cargo fmt --all --check`; `cargo
clippy --all-targets --all-features -- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
--all-features`; `cargo +1.95 check --all-targets --all-features`; the test targets `fdtd`, `fdtd_ems`,
`fdtd_parts`, `fdtd_conformal`, `openems`, `agent_session6`, `agent_ergonomics`, `cli`, `mcp` and the `fdtd`
unit tests in a debug build (also with 0.0.13); in release the `#[ignore]` validations
`sim_sparams_microstrip_validation` (|S21| −1.3 to +5.3 % of D52, D100's figures),
`lossless_line_near_the_pml_is_passive` (0.9991 to 1.0007), `sim_sparams_mask_validation`,
`sim_sparams_parts_validation`, `routed_45_degree_line`, `cavities_match_their_modes` and `rf_path_full_wave`
(|S11| 0.5726, 14 529 steps per port, 30.3 s); the ten Touchstone comparisons with 0.0.12; `solver_comparison`
without openEMS. All pass. The golden driver `tests/golden/openems/via.py` has the thread option's lines; no
snapshot changed. Not run here: the full `cargo test --all-features` (the merger's) and `cargo deny`. With
openEMS, on the branch `ci/ems-013` (the `openems` job alone, the other jobs cut from that branch's workflow):
runs 44002157976, 44006837399, 44013970406 (0.0.12), 44020626931 (0.0.13) and 44030782504 (0.0.14), each with
the nine oracle tests and the comparison passing.
*Not established:* which of the two solvers a measured via or a measured 0603 would agree with (no reference
that is neither; one refinement each); times on an unshared machine, or on a runner with more cores (four
logical CPUs, where openEMS does not gain from threads); memory on Windows or macOS; openEMS at another revision
than the pinned one; whether the 4 % of loss ems reads over D52 on the solid line above 3 GHz is the lumped
ports' or the model's.


### D104. guide 0.0.12 adopted: each connection its passes leave open is negotiated for before its chains, every net near it giving way a little; the rows of fine pitch planned on no board again; the default unchanged (2026-11-17)
guide 0.0.12 is for the first of D101's gaps, the pads of a fine row that the routes round them shut in (guide's
`docs/benchmarks.md`, "PCB routing negotiated"; its `docs/architecture.md`). `guide = { version = "0.0.12",
default-features = false, features = ["pcb"] }`; nothing else in `Cargo.lock` moves.
**What guide 0.0.12 does** in a run that settles (`RouteOptions::negotiate`, its default):
- *Rounds*: the net with the open connection is completed across the routes in its way, a step within a clearance
  of another net's route costing its length again times a price, half at first. The nets it runs into each lose
  what they have within twelve pitches of where they are in another's way and are joined up again there, within
  twenty-four pitches, across the others at that price or around them; what that runs into has its turn in the
  next round. The price rises by 1.6 a round to a thousand, and each step that is in another's way at the end of
  a round makes tracks and shared room there dearer from then on, by cell of two pitches. This is negotiated
  congestion on exact geometry: what is shared is room within a clearance, not a resource.
- *An end with nothing shared*: after twenty-four rounds, thirty-two nets or three million nodes, each net gives
  up the steps it has in another's way, the net the negotiation is for last, and those left in pieces are joined
  up again with nothing shared.
- *Kept or undone as a chain is*: kept if no route is in another's way and the board has less open, by the count
  of D101 (connections and pieces of fill cut off); if not, every net has the copper back that it had, and the
  connection is left to the chains. After eight undone in a row a settling has no more.
- *After each pass*: the settling comes after the first pass and after each pass on the finer lattice, the chains
  after the last alone.
**What the adapter takes from it** (`router::guide`; ROUTER.md, "guide as a backend"):
- *`negotiate` on*, guide's default, set by name.
- *`row_fanout` off on every board again.* D101 turned it on for two layers. With the negotiations it makes no
  more: at 60 s over six orders corne-cherry has 160 open without the rows and 155 with, nrfmicro 19 and 18, and
  the generated stm32-2l has its 72 vias and 1146 mm back for 126 and 1300 mm. `ROW_FANOUT_LAYERS` is gone:
  D101's sixth gap is closed by not needing the rule.
**Measured** (ROUTER.md, "Native router and guide 0.0.12, D104"), 60 s, three orders, 0.0.11 behind D101's adapter
and 0.0.12 behind this one by turns in one queue: 267, 247 and 224 open on the twelve boards → 256, 235 and 217;
the eleven without glasgow-revD1 105, 111 and 101 → 94, 100 and 94. corne-cherry 31, 36 and 33 → 29, 28 and 26;
buspirate5-rev10 0, 6 and 3 → 0, 2 and 0; nrfmicro 3, 5 and 3 → 2, 4 and 3; tinytapeout-demo 13, 7 and 5 → 9, 7
and 5; cynthion 17, 14 and 17 → 13, 15 and 20; glasgow-revC3 one more in one order; the others the same. Zero DRC
errors other than unrouted on every run, no via in a pad of its net, nothing taken out by the verification, no
call over 58.4 s. At 40 million nodes, one run of guide, three orders: 831 → 814 on the nine boards a budget cuts
(corne-cherry 127 → 118, cynthion 63 → 55, buspirate5-rev10 11 → 9, nrfmicro 11 → 9, tinytapeout-demo 38 → 40,
glasgow-revC3 107 → 109). The generated boards: all eight complete with no DRC error with both, seven with the
same routes, stm32-2l with 72 vias for 126.
**The native router's own spread.** The same binary of the native router, three threads, 60 s: glasgow-revD1 had
200 open in D101's hour and 115, 130, 192 and 122 in four runs of this evening, lumenpnp-mobo 15, 9 and 15: where
its minute ends in its negotiation decides. Its twelve boards are 290 in D101 and 200 and 215 here. guide's runs
of glasgow-revD1 are 162, 135 or 136 and 123 every time. "Ahead on glasgow-revD1" in D97 and D101 was against the
native router's worst run; against its five it is within what the native router differs from itself by.
**Tried and not kept**, in guide (its `docs/benchmarks.md`), at 60 s on corne-cherry unless said:
- *The nets in the way routed again whole*, as an FPGA router negotiates: after one net of nrfmicro had crossed
  four, twenty of its forty-one nets were in each other's way, and no fewer at any price.
- *Joined up again from the ends a crossing leaves*, nothing more taken out: no more connections, and tracks that
  keep every bend they ever made.
- *A window of twelve pitches* for twenty-four: 217 over six orders for 159.
- *The rounds ended once the same nets are in each other's way twice in a row*: 82 over three orders for 62.
  *20 000 nodes a search* for 60 000: 84. *A price from one, rising by 2.2*: 83. *Forty rounds*: 77.
- *Two more negotiations for a connection whose first was undone*, each another way: 166 over six orders for 159.
  *The negotiations tried again after each pass*: 152 for 155.
- *A negotiation for each net with pads cut off its fill*: 174 over six orders for 161.
- *The whole board negotiated from nothing in place of the first pass*: 394 of 432 connections after 36 million
  nodes, where the first pass has 390 after 3.4 million.
- *Chains after every pass*: glasgow-revC3 112 at 40 million nodes for 109.
- *A limit of 100 000 or 300 000 nodes on the first pass's searches across routes*, for sweep-v2.2's three nets
  to come to their negotiations: the same 6, 7 and 6.
**Gaps, for guide's next release**, most useful first:
1. *The pads of a QFN of 0.4 mm on two layers with a fan beside them*: corne-cherry has 29, 28 and 26 against 3.
   Some fifteen in an order are pads of its two QFN-56 whose way out needs a dozen parallel tracks moved over by
   one; a negotiation passes that on by one net a round, and its price is at its most after seventeen. The native
   router has every net negotiate from the first iteration, so the fan is spread before it is laid; guide's
   first pass lays it greedily. A negotiated first pass for the nets of such a package, with whole nets routed
   again and a price that rises slowly, is what was not built; the one tried here used the cut-and-join of the
   later negotiations and did not converge.
2. *A second run that goes on from the first run's routes.* Five to ten of corne-cherry's open pads are pads that
   routes cut off a pour, found by cadlab's fill and given to a second run in which nothing moves. guide needs to
   take a run's routes back as routes of their nets, to move, with the host's new fill.
3. *The D+ of a USB receptacle* whose pads have those of D- between them (nrfmicro 2 of its 2 to 4, one or two
   on corne-cherry): the native router joins one pair on the other layer under the pads, with a via at the end
   of each pad. guide's negotiation found it in one build and not as released.
4. *A net with pads that nothing joins to its first* (sweep-v2.2, found after the release). The board is two
   halves that only the schematic joins, through a row of holes that no track passes: three of its
   connections, one each of the ground, `raw` and `vcc`, are open for the native router too, and are all it has
   open. guide grows a net's tree from its first terminal alone, so a net with terminals in both halves joins
   those of one half and leaves every pad of the other on its own: 6, 7 and 6 for 3. A partial route that begins
   another tree from a terminal left out is on guide's branch `forest-wip`, not released: sweep-v2.2 has 3, 3
   and 3 with it, cynthion 35 over three orders for 52 and glasgow-revD1 373 to 408 for 421, and
   tinytapeout-demo 40 to 51 for 21 and buspirate5-rev10 6 or 7 for 1, which is why. Kept to routes that are
   not stubs and to eight pads left out, no board is worse (700 open on nine boards for 711; sweep-v2.2 15,
   cynthion 44) and sweep-v2.2 is not level. What tinytapeout-demo loses by it is the next thing to find.
5. *Six layers in a minute* (glasgow-revD1): its first pass does not end, and no negotiation is reached.
6. *Rule areas with a routed track width*, and *vias* (twice the native router's on lumenpnp-mobo and
   tinytapeout-demo), D97's items, not touched.
**Default: unchanged.** The condition is that guide be at least as good as the native router on every board.
With 0.0.12, three orders against the native router's run of the table: level on three (buspirate-flash-sop,
buspirate-rs232, lumenpnp-ringlight); ahead in every order on two (lumenpnp-mobo 0, 1 and 0 against 15; cynthion
13, 15 and 20 against 28); within what runs differ by on four (buspirate5-rev10 0, 2 and 0 against 2;
tinytapeout-demo 9, 7 and 5 against 6; glasgow-revC3 35, 36 and 34 against 36; glasgow-revD1 162, 135 and 123
against 115 to 200); behind in every order on three (nrfmicro 2, 4 and 3 against 0; sweep-v2.2 6, 7 and 6 against
3; corne-cherry 29, 28 and 26 against 3). 94, 100 and 94 against 93 on the eleven without glasgow-revD1, behind
by 1, 7 and 1 where D101 had 11, 23 and 20; corne-cherry alone is behind by 26, 25 and 23. `router: guide` stays
the second opinion.
*Why:* a chain is a search over orders of moving one net at a time, and what a walled-in pad needs is several
nets each moved a little, which no order of whole moves gives. Negotiation prices that instead of searching it.
It is local and after the fact here, seeded by one connection on routes that are already legal, because that is
what could be added without a run ever ending with less than it had; the cost is that what the first pass laid
greedily is only ever moved by what a single connection can push.
*Not measured:* the native router over seeds; 60 s with the rows on the boards of four layers and the
negotiations; budgets other than 60 s and 40 million nodes; the work-budget table with the released crate (it is
of the release's source); cadlab's oracles; KiCad's DRC on a board guide routed.

### D105. ems 0.0.15: the time step from a bound over the cells, proven part by part and tested from noise; 1.00 to 1.31 times the Courant step on the oracle's regions, where an estimate had read 1.35 to 1.5; the comparison and the corpus tables out of every push (2026-11-17)
D103 left the time step as the largest thing in time: ems took `courant` of the Courant limit of the smallest
cell sizes at the vacuum speed, openEMS's criterion gave 2 to 16 % more on four cases, and "the stability
figures read 0.66 to 0.74, so the scheme would carry a step 1.35 to 1.5 times as long". This change takes what
of that can be proven, says what it rests on part by part, and tests it. No openEMS, CSXCAD or other GPL code was
read.
**What the 0.66 to 0.74 were.** Power iterations on the curl-curl operator: estimates of its largest eigenvalue
from below, converged (1000 and 4000 products agree to four digits), not bounds. Read again on ems 0.0.15 they
put the limit at 1.45 (lossy line, sheets), 1.38 (lossy line, `solid`), 1.29 (via) and 1.40 (parts) times the
Courant limit. A step taken from them would rest on a measurement of each mesh, so it is not taken: the step is
`courant` times a bound.
**The bound** (ems `StepLimit::Cells`, `engine::step_bound`, wave 30 of its design notes; cadlab asks for it in
`fdtd::ems::settings`). The lossless scheme `C u' = Kᵀi`, `L i' = −K u` on leapfrog is stable iff
`dt² λ_max(C⁻¹KᵀL⁻¹K) ≤ 4`. Curl-curl operator and capacitance are sums over the cells, so `λ_max ≤ max λ_c`
over the cells (the element bound of Irons and Treharne, which ems already kept small cut faces by): `λ_c` is
the largest eigenvalue of a cell's own six faces and twelve edges, each edge with its permittivity and its part
of the capacitance, the edges on the region's PEC walls and on PEC copper closed. For one ε and μ it is the
Courant limit of that cell in that medium, exactly. The step is `courant · min 2/√λ_c`, never less than before
(the Courant step where the cells give less) and equal to it on a uniform mesh in vacuum. Every other part, by
its own theory and not by a margin:
- *conductivity, the Debye laminate, lumped R, L, C and ports*: trapezoidal branches coupled implicitly; the
  energy of fields and branches does not grow at any step and is positive under the same condition (the
  laminate counts with ε∞, which is what its edges' capacitance holds);
- *lossy sheets* (rational impedance or ladder): the edge is two face voltages with the capacitances of their
  sides, joined by a passive branch; a face in the sheet's plane reads a convex combination of the two, so the
  split system is bounded by the cells above with one and the cells below with the other. The bound takes a
  sheet edge with its upper side's capacitance in the cells above (air: ε 1) and the lower side's below;
- *lossy `solid` copper*: an edge in the metal's surface is an ordinary edge with the capacitance of the part
  of its dual face outside the metal, at least vacuum's; cells inside the metal have no field;
- *cut cells* (slanted and round copper): in the same bound with their open lengths and areas. The step is
  held so that every cell with a cut face passes twice: as ems's own test sees it, so the faces kept with
  their true area at the Courant step are still kept (D89; the conformal treatment of a model does not change
  with the step), and as the scheme is, within `courant` (an edge that the outline of a lossy sheet cuts is
  its open part in series with the covered part's faces, not a perfect conductor's `C/f`; beside lossy metal
  within √(15/16), which the floor of its surface edges' capacitance there is proven for);
- *the CPML*: no energy proof exists. With frozen coefficients its characteristic polynomial has no root
  outside the unit circle up to the Courant number 1 of the unstretched cell, over κ 1 to 15, σ·dt/ε₀ 0 to 100
  and α·dt/ε₀ 0 to 5 (an ems unit test scans them); the graded layer rests on the long runs below.
Where a model has a part outside the bound, ems keeps the Courant step and says which (`Compiled::step`): of
what cadlab makes, a cut face below the merge threshold inside the absorbing layer, and any mesh with a cut face
too small to keep at the Courant step (it is interpolated at either step, and its cells bound nothing).
**What limits it, and the step gained.** On cadlab's meshes the smallest cells are not at the copper but at the
region's walls: the mesher refines towards every copper edge (`thirds`), and the edge where the region cuts a
plane is one, so the last cells before the wall are 83 µm across where the cells under the strip are 250 µm.
The cell that sets the Courant limit lies against the wall, and the wall closes four of its edges: its own
eigenvalue allows 1.31 times the Courant step on the sheet line, where the closed form (the cell's sizes and its
least ε) finds 1.06. On the `solid` line and the via the smallest cells are the copper's own thickness, in air
beside the copper, with the smallest sizes of all three axes in one cell: there the bound is the Courant limit,
but for what the wall gives.

| Region (`courant` 0.95) | Courant step | Step of the cells | Gained | Steps per port | By the estimate (not taken) |
|---|---|---|---|---|---|
| lossy line, sheets, microstrip ports (`line_msl`) | 0.1949 ps | 0.2544 ps | 1.31× | 6145 → 4737 | 1.45× |
| lossy line, `solid`, lumped ports (`line_lumped`) | 0.0994 ps | 0.1049 ps | 1.06× | 12 065 → 11 425 | 1.38× |
| via transition (`via`, `via_same`) | 0.0545 ps | 0.0545 ps | 1.00× (the whole cells give 1.006, its 532 cut faces' cells as the scheme is 0.79: the Courant step) | 32 961 | 1.29× |
| series 50 Ω, series 1 pF | 0.1689 ps | 0.1792 ps | 1.06× | 7105 → 6689 | 1.40× |
| stripline (`measure_stripline`); lossless `solid` line (`measure_dielectric_loss`) | 0.1013 ps; 0.0994 ps | 0.1077 ps; 0.1049 ps | 1.06× | 9089 → 8545; 12 065 → 11 425 | |
| straight and 45° line, perfect sheets, 1 mm cells and the refined mesh | 0.1689 ps | 0.2299, 0.1983, 0.2036 ps | 1.36×, 1.17×, 1.21× | 7745 → 6433 (conformal), 8929 → 7617 (staircase) | |
| 45° line with lossy sheets or lossy `solid` copper, conformal; its 0.25 mm and 0.3 mm conformal meshes; session 6's RF path | | | 1.00× (the cells with cut faces give less than the Courant step: on the two finer meshes 0.28 and 0.62 of it, where the whole cells give 1.22 and 1.24) | unchanged | |

openEMS's criterion gives 0.1982 ps on the sheet line (ems now 0.2544) and 0.1966 ps on the parts, where ems
proves 0.1886 ps at factor 1 and runs 0.1792: openEMS's step there is above what a single cell proves and below
the estimate. **What is left** is the difference between the bound and the estimate, 1.11 to 1.32 times. The
theorem holds for any partition of the cells into patches, and a mode cannot live in one cell: patches of a few
cells around the worst ones would prove more. Not made here. And a cadlab matter this showed: without the thirds
lines at the walls (copper the region cuts is not an edge) the sheet line's smallest cells would be the 125 µm
of laminate under 174 µm of air, about twice today's step by the same bound, on a mesh with fewer cells; that
changes the mesh of every region and both solvers' results, and is not done here.
**Stability, tested on the scheme as assembled.** A band-limited pulse reaches the grid's highest modes only
through rounding, so ems's tests (`tests/step_limit.rs`) fill every edge of a compiled model with noise, which
has every mode in it, and step it without a source: the energy must stay below its start. Boards with a Debye
laminate, CPML and lumped ports, copper as PEC sheets, ladder sheets, rational sheets and 35 µm of lossy metal:
30 000 steps each at 0.95 and at factor 1, with the CPML and in a closed box; strips at 17° and 45° (38 to 80
cut edges); adversarial meshes, 20 µm cells beside 0.5 mm ones and a laminate of eight 25 µm layers under a
strip at 30°, 40 000 steps; `#[ignore]`: 400 000 steps. A uniform grid of ε 4 is bounded at 0.999 of the bound
(twice the Courant limit) and grows at 1.03: the bound is sharp where it is exact. In cadlab
(`long_runs_stay_bounded`, `tests/openems.rs`, `#[ignore]`): the oracle's five cases, each to its criterion
and then ten times as many steps to a criterion no run reaches; the energy of every progress line must stay
under the criterion after the usual end and not be higher at the end than at twice the usual length. Read:
the sheet line −62.0 dB at twice its 4737 steps and −87.3 dB after 47 361; the `solid` line −62.0 and −88.5 dB
(114 241 steps); the via −63.8 and −85.1 dB (329 601 steps, at the Courant step); the parts −127 dB (66 881
steps). Nothing came back. And every case of the validation table below once more by hand (a hook in the
driver for the measurement, not kept: beside each port's run a second engine of the same compiled model
stepped five times as long, its energy read at the usual end, at twice and at five times that): 60 runs, the
64 mm microstrip, the passive line, the mask, the parts on the finer mesh, the six meshes of the 45° line
(2.46 million cells among them), the cavities, the via, the stripline, the twelve bent lines and session 6's
path. In none is the energy at twice the usual length above that at the usual end, or above the level at twice
later on, but for four runs that wander at their rounding floor, a millionth of the energy they ended on.
**Validation: every case against its reference, 0.0.14 and 0.0.15.** The comparison's rows are 0.0.14 from
crates.io and the new code; the other rows are one binary of the new code on the Courant step, which is
0.0.14's scheme (it reads 0.0.14's figures on the comparison's cases to every printed digit), and on the step
of the cells. Release build, six threads. *What moved the wrong way, first:* seven figures, by one
to three units of the fourth decimal: the 64 mm microstrip's worst \|S21\| against D52 (0.0872 → 0.0875 dB at
9 GHz, 5.26 → 5.28 %; to 2.5 GHz 0.0030 → 0.0032 dB, 1.30 → 1.38 %), the capacitor with ESL and ESR (0.0078 →
0.0079), the series 1 pF of the comparison (0.0099 → 0.0100 and 0.0064 → 0.0065), the stripline's εeff (0.45 →
0.46 %), the lossless `solid` line's lowest point (0.0122 → 0.0123 dB), and the bent line's largest Σ\|S\|² on
perfect copper (staircase 1.0001 → 1.0002, `solid` 1.0000 → 1.0001). Each is inside its test's tolerance by
the margin it had; they are of the size by which a run's last 32 steps move a result, and whether they are that
or the step is not separated. Everything else is equal or nearer:

| Case | Reference | 0.0.14 (Courant step) | 0.0.15 (cells) |
|---|---|---|---|
| lossy line, sheets (`line_msl`): loss from 0.5 GHz | D52 | within 0.0190 dB, RMS 4.3 %, mean +2.6 % | 0.0187 dB, 4.2 %, +2.6 % |
| the same at 0.06 GHz | D52 (0.0072 dB) | 0.0082 dB off | 0.0076 dB off |
| the same, Z₀ | 2D field solver | 0.3 % | 0.3 % |
| lossy line, `solid` (`line_lumped`): loss from 0.5 GHz | D52 | 0.0404 dB, RMS 5.9 %, mean +4.6 % | 0.0403 dB, 5.8 %, +4.6 % |
| the same at 0.06 GHz; Z₀ | D52 (0.0086 dB); field solver | 0.0089 dB off; 5.5 % | 0.0089 dB off; 5.5 % |
| series 50 Ω (comparison) | circuit theory: ideal lines; the lines as the field solver has them | 0.0048; 0.0015 | 0.0048; 0.0015 |
| series 1 pF (comparison) | the same two | 0.0099; 0.0064 | 0.0100; 0.0065 |
| 64 mm lossy microstrip, \|S21\| | D52 | −1.3 to +5.3 %, RMS 2.71 % | −1.4 to +5.3 %, RMS 2.71 % |
| the same, Z₀ and εeff | 2D field solver | within 2.94 % and 0.98 % | 2.94 % and 0.98 % |
| lossless line next to the absorber, Σ\|S\|² | passivity (1) | 0.99908 to 1.00068 | 0.99908 to 1.00068 |
| solder mask: Z₀, εeff; bare εeff | 2D field solver | 50.62 for 50.07, 3.1542 for 3.1623; 3.0837 for 3.0709 | the same digits |
| parts on the finer mesh: 50 Ω, 1 pF, 1 pF with ESL and ESR | circuit theory | 0.0073, 0.0094, 0.0078 | 0.0073, 0.0094, 0.0079 |
| cavities: PEC modes, Q from σ, from copper plates, Debye filling | analytic modes and Q | −0.05 and −0.08 %; +0.04 %; +0.08, −0.14, −0.22 %; resonance −0.01 %, Q −0.65 to −0.74 % | the same digits |
| stripline: Z₀, εeff | 2D field solver | within 1.78 %, 0.45 % | 1.78 %, 0.46 % |
| lossless-copper line with the Debye laminate (`measure_dielectric_loss`), from 0.5 GHz | D52 | within 0.0200 dB | 0.0200 dB |
| routed 45° line, six meshes: S11, S21, phase at 3.1, 5 and 10 GHz | the straight line of its length; the finer meshes | (as printed) | the same digits on all six, four of them on a longer step |
| via, perfect sheets, 0.3 mm cells: \|S21\| at 4 GHz, staircase and conformal | the 0.2 mm mesh (D87: −0.348 dB) | −0.425, −0.376 dB | −0.425, −0.376 dB |
| bent line, six copper models on two meshes: largest Σ\|S\|² | passivity | 1.0003, 1.0001, 0.9979, 0.9979, 1.0000, 0.9983 (1 mm) | 1.0003, 1.0002, 0.9979, 0.9978, 1.0001, 0.9983 |
| session 6's RF path | its ideal chain (not a reference: pads and the via are not in it) | \|S11\| 0.5726, 14 529 steps | the same (Courant step) |
| the removed engine's recorded figures (`RECORDED_*`, `ems_runs_the_region`) | cadlab's own engine at 3aa423f | within 0.01 and 0.015 | within the same tolerances; no constant touched |

**Time and memory beside openEMS.** On the development machine (AMD Threadripper 9970X, one run each, the
comparison without openEMS), the whole `sim.sparams` run, 0.0.14 → 0.0.15: on one thread the sheet line 9.1 →
7.0 s, the `solid` line 17.5 → 17.4 s (16.8 s in another run), the via 63.2 → 62.9 s (the same step), the parts
4.2 and 4.3 → 4.0 and 4.1 s; on four threads 2.7 → 2.0 s, 4.9 → 4.7 s, 23.2 → 22.3 s, 1.4 → 1.2 and 1.1 s. The bound itself
takes 6 to 12 ms of a compile on these regions and 0.2 s on 2.46 million cells. The peak memory is what it was
(41, 46, 61, 34 MB). On the runner, beside openEMS in the same job
(run 44058840773, the new `openems-comparison` job, 48 minutes; an AMD EPYC 7763 this time, four logical CPUs:
a fourth CPU model since D103, so the seconds and the ratios are this runner's and D103's table, on a Xeon
6975P-C, is not their "before"; the steps and the time steps are the same on any machine):

| Case | Steps per port, ems 0.0.14 → 0.0.15 / openEMS | Time step, ems 0.0.14 → 0.0.15 / openEMS (ps) | Whole run on one thread, ems / openEMS | Each at its fastest (threads) | Peak memory | Of it the run |
|---|---|---|---|---|---|---|
| `line_msl` | 6145 → 4737 / 8820 | 0.1949 → 0.2544 / 0.1982 | 15.9 / 48.4 s (3.0×) | 7.9 (4) / 48.4 (1) s (6.1×) | 45 / 75 MB | 22 / 24 MB |
| `line_lumped` | 12 065 → 11 425 / 19 680 | 0.0994 → 0.1049 / 0.1011 | 39.2 / 136.5 s (3.5×) | 19.4 (4) / 128.7 (4) s (6.6×) | 50 / 78 MB | 27 / 27 MB |
| `via` | 32 961 / 27 216 | 0.0545 / 0.0482 | 154.9 / 435.8 s (2.8×) | 89.4 (4) / 391.8 (4) s (4.4×) | 65 / 109 MB | 42 / 58 MB |
| `series_r` | 7105 → 6689 / 8037 | 0.1689 → 0.1792 / 0.1966 | 8.6 / 30.6 s (3.6×) | 4.6 (4) / 26.6 (4) s (5.8×) | 35 / 71 MB | 12 / 20 MB |
| `series_c` | 7105 → 6689 / 8460 | 0.1689 → 0.1792 / 0.1966 | 8.5 / 32.1 s (3.8×) | 4.6 (4) / 27.7 (4) s (6.0×) | 36 / 71 MB | 12 / 20 MB |
| `via_same` | 32 961 / 28 518 | 0.0545 / 0.0460 | 154.6 / 315.8 s (2.0×) | 89.5 (4) / 283.4 (4) s (3.2×) | 66 / 91 MB | 44 / 41 MB |

ems is 2.0 to 3.8 times as fast on one thread and 3.2 to 6.6 times at each solver's fastest on this runner
(with 0.0.14 on the Xeon: 1.5 to 3.2 and 2.7 to 7.0 times; with 0.0.12, 1.4 to 2.4 times on an EPYC 9V45 and
1.8 to 2.9 on the Xeon: the CPU moves the ratio by as much as a release does, and what this release did to
ems's own time is the development machine's row above, the steps' ratio at most). Accuracy beside openEMS, the
references being neither: the lines' loss from 0.5 GHz 0.0187 dB (4.2 % RMS) for openEMS's 0.0268 dB (15.8 %)
and 0.0403 dB (5.8 %) for 0.0815 dB (17.0 %); Z₀ 0.3 for 4.0 % and 5.5 for 5.4 %; at 0.06 GHz 0.0076 and
0.0089 dB for openEMS's 0.0053 and 0.0051 dB; the parts against ideal lines 0.0048 and 0.0100 for 0.0029 and
0.0089, against the lines as they are 0.0015 and 0.0065 for 0.0054 and 0.0058; the via 0.067 dB and, on one
mesh, 0.112 dB from openEMS (0.067 and 0.047 or 0.112 dB with 0.0.14: openEMS ran 28 518 steps on port one
this time). ems's files are the same bytes on one and four threads on every case; openEMS's `solid` line is
not (19 680 and 18 655 steps on port one; 22 221 in D103's runs). *Where openEMS is still ahead or level:* the
0.06 GHz point; the parts against ideal lines, and the capacitor against the lines as they are by 0.0007; the
run's own memory on the via on one mesh (44 for 41 MB) and level on the `solid` line (27 for 27 MB); and its
time step on the parts, 0.1966 for ems's 0.1792 ps.
**Not taken, with the measurement.** *The lowest frequency (D103's item):* an end criterion that knows the
lowest frequency asked for would have to run the −50 dB runs to about −60 dB: the sheet line's 0.06 GHz point
is 0.0076 dB from D52 at −50 dB (4737 steps), 0.0018 dB at −60 dB (8161 steps, 1.72 times) and 0.0009 dB at
−70 dB (16 801, 3.5 times); the `solid` line 0.0089, 0.0023, 0.0011 dB for 11 425, 19 617 and 40 481 steps.
openEMS's 0.004 to 0.005 dB is what its longer run buys (8820 steps where ems took 6145). A criterion that
costs 1.7 times the steps for one point at 1 % of `f_max` is not a gain as a default; `end_criteria: 60dB`
is there for who needs that point, and extrapolating the tail instead of running it is not attempted. *The
run's own memory on the `solid` line and the via, and D98's two items* (`cb`/`db` on meshes graded in the
plane, eight bytes per step and probe): not touched. A Debye edge already holds one charge per pole and shares
its constants between edges of one geometry; what is left is `f64` state, and narrowing it changes results.
**Tried and dropped.** The closed form alone (the cell's sizes with its least ε): rigorous and nearly free, but
it finds 1.00 to 1.06 where the cell's eigenvalue finds 1.00 to 1.31, because it does not see the wall. Lossy
metal's surface edges as vacuum everywhere: it made cells inside the copper the worst cells of the via (they
have no field). A model with any cut face in the absorbing layer kept on the Courant step: the sheet line has cut faces
there, above the merge threshold, where they keep their current as outside the layer; only a face below the
threshold there keeps the model on the Courant step now. The step from the power iteration: above.
**The CI jobs.** The `openems` job had grown from 13 to 45–60 minutes with the comparison, on every push, and
`freerouting` took 46. Each is two jobs now: `openems` (the nine oracle tests) and `freerouting` (the DSN and
session oracle of `tests/specctra.rs`, which needs the jar and Java but neither the corpus nor `route_bench`)
stay on every push; `openems-comparison` (with the `openems_fine` input) and `freerouting-corpus` (the four
tables) run on the weekly schedule (Monday 03:17 UTC) and on a manual dispatch only. One dispatch makes both
tables again: `gh workflow run CI --ref master`. The schedule runs the other jobs too, once a week. ROUTER.md's
sentence on the job that makes its table means `freerouting-corpus` now (that file is another change's).
**Verification.** ems: `scripts/check.sh` (fmt, clippy and the tests with all features and with none, docs,
`cargo +1.95 check`) passed before the merge (13 minutes, six build jobs, four test threads); the release PR (ems PR 15) had Test & lint on Linux, Windows and macOS, Docs and MSRV green, and master's CI
was green, before `gh pr merge`; 0.0.15 is on crates.io.
cadlab (Linux x86-64, ems 0.0.15 from crates.io): `cargo fmt --all --check`; `cargo clippy --all-targets
--all-features -- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`; the test
targets `fdtd`, `fdtd_ems`, `fdtd_parts`, `fdtd_conformal`, `openems`, `agent_session6`, `agent_ergonomics`,
`cli`, `mcp` and the `fdtd` unit tests in a debug build; in release the `#[ignore]` validations of the table,
`long_runs_stay_bounded` and `solver_comparison` without openEMS. All pass; no golden file or snapshot changed
(the export's mesh and driver are untouched, and no figure of a snapshot comes from a run). Not run here: the
full `cargo test --all-features` (the merger's), `cargo deny`, the MSRV check of cadlab. With openEMS, on the branch
`ci/ems-015` (the two openEMS jobs alone, the other jobs cut from that branch's workflow), run 44058840773
(https://github.com/KarpelesLab/cadlab/actions/runs/44058840773), ems 0.0.15 from crates.io: `openems`, the
nine oracle tests, passes in 14 minutes (13 before D103 added the comparison to it, 45 to 60 with it);
`openems-comparison` passes in 48 minutes. The schedule trigger and the `freerouting` and
`freerouting-corpus` jobs have not run: the workflow file parses (it was dispatched from the branch with the
same triggers and conditions), and the two freerouting jobs are the old job's steps divided, unrun.
*Not established:* whether the seven figures that moved by 0.0001 to 0.0003 are the step or the run's last
steps; a bound over patches of cells and how much of the estimated 1.29 to 1.45 it proves; the CPML beyond
frozen coefficients and long runs (no proof for the graded layer is known to me); stability from noise on
cadlab's own regions (cadlab's long runs are pulses; the noise runs are ems's boards); times on an unshared
machine.

### D106. guide 0.0.13 and 0.0.14 adopted: a route that keeps what it reaches begins another tree from a terminal it leaves out; a last run takes the routes of the runs before back as routes that move; level or ahead is said by ranges of four runs; the default unchanged (2026-11-17)
This round is for D104's gaps, a release of guide at a time (guide's `docs/benchmarks.md`, "PCB routing of a board
in two parts"; its `docs/architecture.md`). `guide = { version = "0.0.13", default-features = false, features =
["pcb"] }`; nothing else in `Cargo.lock` moves.
**The basis of every comparison from here on.** D104 found that the native router's own runs of glasgow-revD1 are
115 to 200 open. So both routers are measured over four runs in one hour, the native router with seeds 1 to 4 on
three threads, guide over four orders of its nets (no seed, 2, 3, 4) on one thread, and "level", "ahead" and
"behind" are said of the ranges, least / median / most (ROUTER.md, "Native router and guide 0.0.13, D106"). The
figures at 40 million nodes stay beside them: they are the same on any machine.
**What guide 0.0.13 does** (D104's fourth gap, its branch `forest-wip` made releasable):
- *Another tree.* In a run with a budget, a route that keeps what it reaches (a net with a plane, one of more
  than 64 pads, or any net with `keep_partial`) and leaves two to eight terminals out begins another tree from one
  of them, twice at most; the net's pieces are counted from its copper. A net with pads in both halves of
  sweep-v2.2, which only the schematic joins, has the pads of each half joined and one connection open, where
  every pad of the half without its first terminal was left on its own.
- *Not in one search*: the one that a net has with nothing moved in the first pass, before its turn to cross
  other nets' routes. This is what the branch cost tinytapeout-demo: what that search leaves is copper that waits
  for the net's next turn, and pads joined among themselves that the first terminal reaches none of were then in
  the way of nets that had room without them. Found by turning the trees off search by search at 60 s over six
  orders: 91 open with them everywhere for 59 with 0.0.12; 81 without those of the nets with a plane, 85 without
  those for the pads cut off a fill, 81 without both; 60 with none in the searches with nothing moved; 60 with
  none in that one search alone, which is the release. buspirate5-rev10's loss on the branch was between two
  runs of a board that differs by as much from itself (1, 7, 7 and 4 with 0.0.12 in this hour; 0, 5, 4 and 0
  with 0.0.13).
**What the adapter takes from it:** nothing to set: the trees are of any run with a budget.
**Measured** (ROUTER.md, "Native router and guide 0.0.13, D106"), 60 s, four orders, 0.0.12 and 0.0.13 behind the
same adapter: the twelve boards 237 to 267 open (median 239) → 218 to 259 (223); the eleven without glasgow-revD1
104 to 116 (109) → 91 to 98 (95.5), against the native router's 80 to 107 (89) over its four runs. sweep-v2.2 6,
7, 6 and 7 → 3, 3, 3 and 3; cynthion 79 over the four → 67, glasgow-revC3 147 → 133, buspirate5-rev10 19 → 9,
corne-cherry 115 → 107, tinytapeout-demo 38 → 37, nrfmicro 12 → 13, lumenpnp-mobo 2 and glasgow-revD1 544 → 543
the same runs. Zero DRC errors other than unrouted, no via in a pad of its net, nothing taken out by the
verification, no call over 58.2 s. At 40 million nodes, three orders: 814 → 801 on the nine boards a budget cuts
(corne-cherry 118 → 108, glasgow-revC3 109 → 104, sweep-v2.2 19 → 16, buspirate5-rev10 9 → 12, nrfmicro 9 → 10,
cynthion 55 → 56, the others the same); the released crate gives the figures of its source on the two boards
run with it (nrfmicro, corne-cherry). guide's curated
boards are as they were, 1150 of 1151 connections with 712 vias.
**Tried and not kept**, in guide: the trees in every search (above); kept to routes that are not stubs and to
eight pads left out (D104: sweep-v2.2 15 over three orders, not level). On glasgow-revD1 the trees in every search
had 373 to 408 open over three orders for 421, which the release does not have: its minute ends in the first
pass, where pads joined among themselves early are connections made.
**Default: unchanged.** By the ranges: level on four boards (buspirate-flash-sop, buspirate-rs232,
lumenpnp-ringlight, sweep-v2.2); ahead in every run on two (lumenpnp-mobo 0 to 1 against 8 to 29; cynthion 14 to
23 against 28 to 29); ranges that overlap on four (buspirate5-rev10 0 to 5 against 0 to 5; glasgow-revC3 31 to 35
against 31 to 36; glasgow-revD1 121 to 162 against 118 to 192; tinytapeout-demo 5 to 16 against 3 to 8, medians 8
and 5.5); behind in every run on two (nrfmicro 2 to 5 against 0; corne-cherry 21 to 33 against 0 to 4). The
condition, at least as good on every board, is not met on those two, nor shown on tinytapeout-demo.
`router: guide` stays the second opinion.
*Why:* a tree that grows from one terminal says "not reached" of every pad beyond a gap that no track passes, and
the count of what is open then says how many pads are beyond it, not how many connections are missing. The
trees are kept out of the one search whose result waits as copper, because there they were laid before the nets
that had room, on a board where every net has a way with them out of it.
*Not measured:* the native router over more than four seeds; guide over more than four orders at 60 s on the
whole corpus; the generated boards (a run with no budget begins no second tree); cadlab's oracles; KiCad's DRC on
a board guide routed.

**Second release of the round: guide 0.0.14** (guide's `docs/benchmarks.md`, "PCB routing from an earlier run's
routes"). `guide = { version = "0.0.14", ... }`; nothing else in `Cargo.lock` moves. It is for D104's second gap,
and has what was built for its first.
**What guide 0.0.14 adds:**
- *`pcb::route_from`*: a run that begins with the tracks and vias of an earlier run as routes of their nets,
  which it may move. A net that they join whole has no turn until another net moves it; one in pieces is
  completed from them with the turns of a first pass (nothing moved until every net has had one). The stubs of
  a net with a plane are taken back with a place in the fill for each end that a piece of it still holds. A
  track of another width than its net's, an arc, a via of another size or a net with nothing to join stays
  fixed; `Routed::taken_back` lists the nets taken. A net that the run does not move is not cleaned up again.
- *`RouteOptions::negotiate_places`*, off by default: after the first pass, each place where two pads or more
  with nothing joined to them are within twelve pitches of each other has one negotiation among all the nets of
  at most eight terminals there. Each loses all it has in the place in every round that it is in another's way
  in, and the price rises by a quarter a round, for forty-eight rounds at most. This is D104's first gap as it
  was put there: whole routes again, a price that rises slowly, kept to the place. It settles.
**What the adapter takes from it** (`router::guide`; ROUTER.md, "guide as a backend", "A last run that moves
routes"):
- *A third run*, after the second on the fixed board, in what time or work that leaves (3 s at least): every net
  left open, and the routes of the runs before handed back as routes (`Adapted::take_back`). Its result stands
  in place of the routes of the nets guide took back; kept if the board has fewer connections open.
- *`negotiate_places` left off*: no gain at 60 s (below).
**Measured** (ROUTER.md, "Native router and guide 0.0.14, D106"), 60 s, four orders, 0.0.13 behind the adapter
with two runs and 0.0.14's source behind the one with three, six runs at a time: 923 open over the four orders on
the twelve boards → 890 (259, 227, 218 and 219 → 241, 223, 205 and 221); the eleven without glasgow-revD1 97, 91,
94 and 98 → 79, 87, 81 and 97. corne-cherry 107 → 87, buspirate5-rev10 9 → 1, tinytapeout-demo 37 → 33, cynthion
67 → 64, lumenpnp-mobo 2 → 1; nrfmicro 13, sweep-v2.2 12 and glasgow-revC3 133 the same; glasgow-revD1 543 → 546,
three more in one order of its first run. No board has more open in an order for the third run: it is only kept
if it leaves less. Zero DRC errors other than unrouted, no via in a pad of its net, nothing taken out by the
verification, no call over 59.1 s. At 40 million nodes a run that the budget stops is the same to the track
(corne-cherry 38, 38 and 32 with the released crate), and the runs after one that ends within it go on within
the budget (tinytapeout-demo 16, 19 and 1 for 16, 19 and 5). guide's curated boards: 1150 of 1151 connections
with 712 vias, as before.
**Both routers over four runs in one hour, with 0.0.14** (the same table): the twelve boards 215 to 244 open for
guide (median 228) against 197 to 292 for the native router (285); the eleven without glasgow-revD1 82 to 104
(89) against 82 to 92 (88.5).
**Tried and not kept** (guide's `docs/benchmarks.md`), at 60 s over four orders, where the corpus has 923 open
with 0.0.13:
- *The second run itself from the first run's routes*, in place of the run on the fixed board: sweep-v2.2 15 to
  21 over the four orders for 12 (searches across routes for its three connections that no route makes, and
  the deadline before it has joined what the run on the fixed board joins), lumenpnp-mobo 5 for 2.
- *Every net cleaned up again at the end of such a run*: lumenpnp-mobo 5 to 10 over four orders for 1. A route
  pulled tight lies where cadlab's fill has copper, and the fill after it is cut where it was whole.
- *Only the nets without a plane taken back*: corne-cherry 114 for 88 to 92 with all of them, whose zones are
  tracks as wide as a pad; 926 on the nine boards.
- *The nets completed as nets that are mended are*, across other nets' routes at once: sweep-v2.2 21 for 16.
- *The negotiations for places at 60 s*: corne-cherry 71, 84 and 87 in three builds for 87 without, cynthion 75
  for 64, sweep-v2.2 14 for 12, nrfmicro 10 for 13. Three or four places take the nodes of the first run. With
  four minutes: corne-cherry 9 and 11 open in two orders for 13 and 16, 8 in an earlier build.
- *A place's nets kept to its window*: the nets with a pad open have their other pad far off and stay open
  through every round. *Sixteen nets more than it begins with* at most: ended in the second round, as nets
  routed over the board run into others.
**Gaps, for guide's next release**, most useful first:
1. *A negotiation for a place that a minute can pay for.* corne-cherry has 15 to 27 open against 0 to 4. A place
   has thirty nets, each routed again in every round it is in another's way in: 300 000 to 450 000 nodes a round
   at first, three to eight million a place, where the whole first run has thirty to forty. What would make it
   cheaper was not built: searches kept to the place for nets that are whole (their ends at its edge are where
   to join), a price that rises faster once few nets are left, and the places before the first pass has laid
   the fans that they then take apart.
2. *The D+ of a USB receptacle* whose pads have those of D- between them (nrfmicro, all of its 2 to 5; one or two
   on corne-cherry): not touched. A place's negotiation takes two nets of two pads as it takes any, and the
   receptacle's pads are a place only when two of them are open.
3. *Six layers in a minute* (glasgow-revD1): not touched; its first pass does not end, so no negotiation and
   no run after the first has a turn that counts. The range is 125 to 162 against the native router's 115 to 207.
4. *Vias* (lumenpnp-mobo 712 against 347, tinytapeout-demo 605 against 274) and *rule areas with a routed track
   width*: not touched.
5. *sweep-v2.2 at its deadline*: 3 in every order on a machine with four runs at a time, 3, 4, 4 and 4 with ten
   threads beside it. Its first pass spends its time on three connections that no route makes; a run that knew
   a board in two parts would not look for them.
**Default: unchanged.** By the ranges of four runs of each in one hour, with 0.0.14: level on three boards
(buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight) and on sweep-v2.2 off a loaded machine; ahead in every
run on two (lumenpnp-mobo 0 to 1 against 4 to 24; cynthion 15 to 23 against 28 to 32); ranges that overlap on four
(buspirate5-rev10 0 to 2 against 0 to 5; glasgow-revC3 30 to 38 against 31 to 35; glasgow-revD1 125 to 162 against
115 to 207; tinytapeout-demo 1 to 16 against 3 to 6, medians 8 and 4.5); behind in every run on two (nrfmicro 2
to 5 against 0; corne-cherry 15 to 27 against 0 to 4). The condition is not met. `router: guide` stays the second
opinion, and the native router stays.
*Why a third run and not a second that moves:* the run on the fixed board is what repairs the pads that the
first run's routes cut off a pour, which only cadlab's fill sees, and it is cheap; a run that moves routes makes
such cuts again and has no run after it to repair them. So it comes last, and only counts if cadlab's own count
is better for it.
*Not measured:* `negotiate_places` on other boards than corne-cherry with more than a minute; the generated
boards with 0.0.14 (a run with no budget is as it was: the curated boards are); the three runs under a work
budget on the whole corpus; cadlab's oracles; KiCad's DRC on a board guide routed.

### D107. ems 0.0.16 and 0.0.17: the cells' shares balanced, the cut cells in the bound, 1.22 to 1.41 times the Courant step on the oracle's regions where 0.0.15 had 1.00 to 1.31; what the end criterion costs, measured apart from the step; the mesh at the region's walls evaluated and not taken (2026-11-17)
D105 left five things: the part of the estimated limit that single cells do not prove, the thirds lines at the
region's walls, the run's own memory, the lowest frequency of a run to −50 dB, and seven figures that had moved
by one to three units of the fourth decimal without it being known whether the step or the end criterion moved
them. This is what each came to, in that order. No openEMS, CSXCAD or other GPL code was read.
**1. The bound.** The theorem of D105 (`λ_max ≤ max_c λ_c`, Irons and Treharne) holds for any way of dividing an
edge's capacitance among the cells around it and a face's dual edge between its two cells, as long as the parts
add up and none is negative. D105 took the geometric division (a quarter of the dual face, half of the dual
edge), with which a thin cell between thick ones has the Courant limit of its own sizes, because it has little
of the edges it shares. ems 0.0.16 (`engine::step_bound`, wave 31 of its design notes) divides the dual length
at every node of every axis between the cell below and the cell above in parts of its choosing, one number per
node and axis. A cell is then the cell of a grid whose dual lengths at its two ends are its shares `lo`, `hi`,
and for one ε and μ its eigenvalue is at most `Σ_a (1/lo_a + 1/hi_a)/(Δ_a·εμ)` (the cell is the product of
three elements of two nodes; curl-curl plus the gradient of the divergence is the sum of their three second
differences, and the gradient part is not negative); an end that a wall or a conductor closes drops its term.
On a graded mesh in one medium between walls this family contains the limit itself: the operator is the sum of
three chains whose largest eigenvalues add up, and a tridiagonal matrix is the sum of its elements' for the
shares its eigenvector gives. An ems unit test holds the bound to a power iteration on such a mesh within a
part in 10⁴, where the geometric shares gave the Courant limit, 1.65 times shorter. The shares are found one
axis at a time (the least level at which a walk along the axis can give every cell what its closed form asks,
by bisection: what a cell asks is convex in its two shares; a cell whose own eigenvalue is below its closed
form counts by that eigenvalue at the shares of the round before; two to four rounds, 35 to 70 ms on these
regions). Whatever the search finds, the step is `courant` over the root of the largest of the cells'
eigenvalues with those shares, every cell solved or bounded by a closed form that is its eigenvalue: a bound
by the same proof, and never above that of the geometric shares, which is where the search starts.
What does not follow the shares: the plane of a sheet keeps the halves (a face in it reads `f+·u+ + f−·u−` by
the halves, and `(f+·a + f−·b)² ≤ f+²/g+·a² + f−²/g−·b²` for other shares `g±` costs both sides), and an edge
of lossy metal has its capacitance outside the metal, which the cells with field divide by their shares.
**The cut cells, which held the via.** D105 read 0.79 of the Courant limit for the via's cut cells "as the
scheme is" and kept the Courant step there. Three things were in that figure, none of them the scheme. (a) An
edge of lossy metal beside a cut cell has a floor of capacitance in ems, `C ≥ dt²·Σ n/L/(4θ)` over its faces,
θ = 1/16, which is there so that the edge answers for itself: `(s + Σ_n u)² ≤ s²/(1 − θ) + n·Σ_n u²/θ` for a
face's circulation. In the bound such an edge is therefore closed and its faces a sixteenth heavier, in every
cell they are in; D105 had it open with vacuum's capacitance and a margin of √(15/16) on top. (b) The edges the
metal's outline cuts are ordinary edges in ems (their covered part a perfect conductor); D105 counted them as
vacuum's. (c) ems's own test of which small faces keep their current was made at the engine's step, so a
longer step would have changed the conformal treatment, and the step was held to where it did not. That test
is made at the Courant step now wherever the cells give a longer one (`ConformalOptions::keep_step`), so a
model keeps the faces it keeps at the Courant step whatever its step (an ems test compares the two), and the
cut cells are in the bound with the whole cells, as the scheme has them, with the shares of their slabs. The
via reads 1.04 with the geometric shares and 1.22 balanced; a model one of whose cut faces fails that test at
the Courant step (it is interpolated at any step, and its cells bound nothing) stays on the Courant step, as
does session 6's path.
**The CPML** has no energy proof, as before (D105): frozen coefficients, a unit test of the characteristic
polynomial, and long runs.

| Region (`courant` 0.95) | Courant step | 0.0.15 | 0.0.16 and 0.0.17 | Of the Courant step | Steps per port, Courant → 0.0.15 → now | The estimate (kernel alone, not a bound) |
|---|---|---|---|---|---|---|
| lossy line, sheets, microstrip ports (`line_msl`) | 0.1949 ps | 0.2544 ps | 0.2742 ps | 1.41× | 6145 → 4737 → 4385 | 1.45× |
| lossy line, `solid`, lumped ports (`line_lumped`) | 0.0994 ps | 0.1049 ps | 0.1338 ps | 1.35× | 12 065 → 11 425 → 8961 | 1.38× |
| via transition (`via`, `via_same`) | 0.0545 ps | 0.0545 ps | 0.0664 ps | 1.22× (its cut cells; the whole cells alone 1.28) | 32 961 → 32 961 → 27 041 | 1.29× |
| series 50 Ω, series 1 pF | 0.1689 ps | 0.1792 ps | 0.2356 ps | 1.39× | 7105 → 6689 → 5089 | 1.40× |
| 64 mm lossy microstrip, `solid` (`sim_sparams_microstrip_validation`) | 0.0994 ps | 0.1049 ps | 0.1338 ps | 1.35× | 21 153 → 20 033 → 15 713 | |
| stripline with its via fence (`measure_stripline`) | 0.1013 ps | 0.1077 ps | 0.1192 ps | 1.18× | 9089 → 8545 → 7713 | |
| lossless line, sheets (`measure_microstrip`); parts on the finer mesh | 0.1949; 0.1689 ps | 0.2546; 0.2410 ps | 0.2782; 0.2744 ps | 1.43×; 1.62× | 5217 → 4001 → 3649; 7745 → 5441 → 4769 | |
| bent line (`measure_diagonal_copper`): staircase with perfect and lossy sheets on two meshes; conformal with lossy sheets and lossy `solid` on the default mesh | | | | 1.20 to 1.23×, 1.06× and 1.20× of 0.0.15's | 7617 → 6177, 7809 → 6337, 6465 → 5377, 6817 → 5665; 7777 → 7329; 13 729 → 11 457 | |
| 45° line, conformal, perfect copper, six meshes, and the bent line's other six models; via on perfect sheets; session 6's RF path | | | as 0.0.15 | (the worst cut cell is one that shares of a whole slab do not reach; session 6: Courant) | unchanged | |

openEMS's criterion gives 0.1982 ps on the sheet line (ems 0.2742), 0.1011 on the `solid` line (0.1338), 0.1966
on the parts (0.2356) and 0.0482 or 0.0460 on the via (0.0664): ems's step is the longer on every case now.
On boards of perfect copper the bound is the limit to four digits (a power iteration reads 0.9996 to 1.0000 of
it in ems's `the_bound_is_not_above_the_estimate`), so `courant` is the whole margin there, where D105's bound
had ten to thirty per cent of its own: at factor 1 in a closed box the highest mode grows with the step count,
as it does at the Courant limit of a uniform mesh (26 times the energy in 30 000 steps), and at 0.999 nothing
does. cadlab's 0.95 stands.
**Stability, tested.** ems (`tests/step_limit.rs`, noise on every edge, no source): the boards of D105 at the
new steps (1.68 to 2.77 times their Courant steps), 30 000 steps each with the CPML at factor 1 and in a closed
box at 0.999; strips at 17°, 30° and 45° with their cut cells in the bound, 40 000 steps, the 30° strips at the
limit too; `#[ignore]`, run once on the released code: 400 000 steps on eight boards (perfect, ladder,
rational sheets and lossy metal, on the mesh and at 30°), every energy below its start, 3.4 to 5.2·10⁻¹¹ J at
the end from 7.9 to 8.6·10⁻¹¹. cadlab (`long_runs_stay_bounded`, `#[ignore]`, 4 min 39 s on four threads): the
oracle's five cases to their criterion and then ten times as many steps, the energy of every progress line
under the criterion after the usual end and no higher at the end than at twice the usual length. Read: the
sheet line −62.0 dB at twice its 4385 steps and −87.3 dB after 43 841; the `solid` line −62.0 and −88.6 dB
(89 601 steps); the via −63.8 and −85.3 dB (270 401 steps at 0.0664 ps); the parts −128 dB (50 881 steps).
Nothing came back. Not done again: D105's sixty runs by hand at five times the usual length.
**Validation: every case against its reference, 0.0.15 and 0.0.16** (0.0.17 gives 0.0.16's Touchstone files
byte for byte, below). Both columns are one harness on two worktrees, ems from crates.io, release, four threads
(the base at three). *What moved the wrong way, first:* the 64 mm microstrip's worst \|S21\| against D52,
0.0875 → 0.0901 dB at 9 GHz (5.28 → 5.44 %; RMS over its seventeen points 2.79 → 2.85 %); the sheet line's
loss, largest 0.0187 → 0.0189 dB and mean +2.6 → +2.7 %, and its 0.06 GHz point, 0.0076 → 0.0079 dB off; the
`solid` line's RMS 5.8 → 5.9 %, mean +4.6 → +4.7 %, 0.06 GHz point 0.0089 → 0.0090 dB off; the lossless
`solid` line from 0.5 GHz, 0.0200 → 0.0201 dB; the lossless sheet line's Σ\|S\|² (`measure_microstrip`),
0.99845 to 1.00070 → 0.99843 to 1.00076. Each is inside its test's tolerance by the margin it had. Section 5
says which of them the step moved: one.

| Case | Reference | 0.0.15 | 0.0.16 |
|---|---|---|---|
| lossy line, sheets (`line_msl`): loss from 0.5 GHz | D52 | within 0.0187 dB, RMS 4.2 %, mean +2.6 % | 0.0189 dB, 4.2 %, +2.7 % |
| the same at 0.06 GHz; Z₀ | D52 (0.0072 dB); 2D field solver | 0.0076 dB off; 0.3 % | 0.0079 dB off; 0.3 % |
| lossy line, `solid` (`line_lumped`): loss from 0.5 GHz | D52 | 0.0403 dB, RMS 5.8 %, mean +4.6 % | 0.0403 dB, 5.9 %, +4.7 % |
| the same at 0.06 GHz; Z₀ | D52 (0.0086 dB); field solver | 0.0089 dB off; 5.5 % | 0.0090 dB off; 5.5 % |
| series 50 Ω (comparison) | circuit theory: ideal lines; the lines as the field solver has them | 0.0048; 0.0015 | 0.0048; 0.0015 |
| series 1 pF (comparison) | the same two | 0.0100; 0.0065 | 0.0100; 0.0064 |
| 64 mm lossy microstrip, \|S21\| | D52 | −1.38 to +5.28 %, RMS 2.79 %, worst 0.0875 dB; to 2.5 GHz 0.0043 dB | −1.38 to +5.44 %, RMS 2.85 %, worst 0.0901 dB; to 2.5 GHz 0.0042 dB |
| the same, Z₀ and εeff | 2D field solver | within 2.94 % and 0.98 % | 2.94 % and 0.98 % |
| lossless line next to the absorber, Σ\|S\|² | passivity (1) | 0.99908 to 1.00068 | 0.99908 to 1.00066 |
| solder mask: Z₀, εeff; bare εeff | 2D field solver | the test's tolerances hold (it prints no figure) | hold |
| parts on the finer mesh: 50 Ω, 1 pF, 1 pF with ESL and ESR | circuit theory | 0.0073, 0.0094, 0.0079 | 0.0073, 0.0094, 0.0078 |
| the parts of `measure_parts`: R, C, RLC, 0.5 to 4.5 GHz | circuit theory | 0.0048, 0.0100, 0.0077 | the same |
| cavities: PEC modes, Q from σ, from copper plates, Debye filling | analytic modes and Q | −0.08 %; +0.04 %; +0.08, −0.14, −0.22 %; resonance −0.01 %, Q −0.65 to −0.74 % | the same lines (uniform meshes: the Courant step) |
| stripline: Z₀, εeff | 2D field solver | within 1.78 %, 0.457 % | 1.78 %, 0.457 % |
| lossless-copper `solid` line with the Debye laminate, from 0.5 GHz; at 0.06 GHz | D52 | within 0.0200 dB; 0.0123 dB off | 0.0201 dB; 0.0121 dB off |
| lossless sheet line (`measure_microstrip`): Z₀, εeff, Σ\|S\|² | 2D field solver; passivity | 1.22 %, 0.59 %; 0.99845 to 1.00070 | 1.22 %, 0.59 %; 0.99843 to 1.00076 |
| routed 45° line, six meshes; the 45° line of `measure_diagonal`; the via on perfect sheets (`measure_via_convergence`, −0.376 dB at 4 GHz) | the straight line of its length; the finer meshes; the 0.2 mm mesh | (as printed) | the same lines, every digit |
| bent line, six copper models on two meshes: largest Σ\|S\|² | passivity | 1.0003, 1.0002, 0.9979, 0.9978, 1.0001, 0.9983; 1.0001, 1.0006, 0.9977, 0.9979, 1.0001, 0.9983 | 1.0003, 1.0001, then the same ten |
| session 6's RF path | its ideal chain (not a reference) | \|S11\| 0.5726, 14 529 steps | the same (Courant step) |
| the removed engine's recorded figures (`RECORDED_*`, `ems_runs_the_region`) | cadlab's own engine at 3aa423f | within 0.01 and 0.015 | within the same tolerances; no constant touched |

**2. The mesh at the region's walls: evaluated, not taken.** The change: an edge of a copper shape that lies on
a side of the window is where the window cuts the copper, not where the copper ends, so `thirds` leaves it
alone (two conditions in `openems::thirds`). It takes a fifth of the cells of a line region away and the cells
that set its step: the sheet line 67 200 → 53 760 cells and 0.2742 → 0.3443 ps (4385 → 3489 steps), the parts
69 312 → 55 296 cells and 0.2356 → 0.3670 ps (5089 → 3265 steps), the `solid` line 85 320 → 68 256 cells on
the step it had, the via unchanged (no copper on its window's sides), the conformal 45° lines 0.2036 → 0.2360
and 0.1689 → 0.2040 ps. Against the references, on ems 0.0.16, to each case's own criterion and again to
−80 dB, where the end of the run moves nothing (section 5):

| Case | Reference | thirds at the walls (today) | without them | The same two to −80 dB |
|---|---|---|---|---|
| 64 mm lossy microstrip: worst \|S21\|, RMS; Z₀ | D52; field solver | 0.0901 dB, 2.85 %; 2.94 % | 0.0829 dB, 2.68 %; 2.90 % | 0.0925, 2.87 %, 2.94 % → 0.0854, 2.71 %, 2.90 % |
| parts on the finer mesh | circuit theory | 0.0073, 0.0094, 0.0078 | 0.0072, 0.0089, 0.0073 | 0.0073, 0.0095, 0.0078 → 0.0072, 0.0090, 0.0073 |
| `measure_parts` R, C, RLC | circuit theory | 0.0048, 0.0100, 0.0077 | 0.0047, 0.0098, 0.0075 | 0.0048, 0.0093, 0.0077 → 0.0047, 0.0091, 0.0075 |
| series 1 pF (comparison) | ideal lines; lines as they are | 0.0100; 0.0064 | 0.0098; 0.0065 | 0.0093; 0.0029 → 0.0091; 0.0027 |
| series 50 Ω (comparison) | the same | 0.0048; 0.0015 | 0.0047; 0.0014 | |
| sheet line: loss; 0.06 GHz | D52 | 0.0189 dB, 4.2 %, +2.7 %; 0.0079 off | 0.0189 dB, 4.2 %, +2.6 %; 0.0082 off | 0.0165 dB, 0.0007 off → 0.0164 dB, 0.0008 off |
| `solid` line: loss; 0.06 GHz | D52 | 0.0403 dB, 5.9 %, +4.7 %; 0.0090 off | 0.0403 dB, 5.9 %, +4.7 %; 0.0089 off | 0.0394 dB → 0.0393 dB; 0.0007 off both |
| lossless `solid` line with the Debye laminate | D52 | 0.0201 dB; 0.0121 off | 0.0200 dB; 0.0120 off | 0.0164 → 0.0163 dB |
| lossless line next to the absorber, Σ\|S\|² | passivity | 0.99908 to 1.00066 | 0.99909 to 1.00069 | 0.99917 to 1.00000 → 0.99918 to 1.00000 |
| stripline: Z₀, εeff | field solver | 1.78 %, 0.457 % | 1.78 %, 0.460 % | 1.85 %, 0.450 % both |
| **lossless sheet line (`measure_microstrip`): Z₀**; εeff; Σ\|S\|² | field solver (50.81 Ω) | **1.22 %** (50.73 to 50.19 Ω over six frequencies); 0.59 %; 0.99843 to 1.00076 | **1.26 %** (50.72 to 50.17 Ω); 0.59 %; 0.99844 to 1.00073 | **1.20 % → 1.24 %**, 0.02 Ω lower at every frequency; the rest equal |
| bent line, largest Σ\|S\|² (twelve models) | passivity | 1.0003, 1.0001, 0.9979, 0.9978, 1.0001, 0.9983; 1.0001, 1.0006, 0.9977, 0.9979, 1.0001, 0.9983 | 1.0004, 1.0002, 0.9978, 0.9979, 1.0001, 0.9982; 1.0002, 1.0007, 0.9977, 0.9980, 1.0001, 0.9983 | not run |
| routed 45° line: S11, S21, phase at 3.1, 5, 10 GHz on six meshes | the straight line; the finer meshes | (as printed) | S11 within 0.2 dB of it, the phases within 0.1°, S21 the same digits | not run |

Of the eleven rows run to −80 dB, nine are nearer or equal once the end of the run is out of them, some by
much (the 64 mm line's worst point by 0.007 dB of 0.09, the capacitor by 0.0005 of 0.0094). Two are not: the
lossless sheet line's Z₀ reads 0.02 Ω lower at every frequency, 1.20 → 1.24 % from the field solver at its
worst point, to either criterion; and the sheet line's 0.06 GHz point is 0.0001 dB further at −80 dB. The
bent line's passivity, to its own criterion only, moves by 0.0001 on eight of twelve models, six of them
away from 1. The condition was every validation as near or nearer, so it is not taken. Two more things it would need: `ems_runs_the_region` holds
the removed engine's recorded figures on the mesh they were recorded on (27 × 19 × 15 lines, asserted), which
this changes, so that reference would end; and the openEMS oracle shares the mesh and was not run on it.
The two lines of the change are in this section, and nothing of it is in the tree.
**3. Memory.** The heap of a run of the via transition, block by block (valgrind's massif, one thread, 41.3 MB
at a time of which 6.7 MB the allocator's own): the largest single block was not in the engine but beside it,
the compiled model's list of the laminate's poles, kept for the next port's engine (16 bytes an entry, 387 000
entries: 6.2 MB); then the Debye state (2.6 MB each for the charges and the two constants of 320 000 poles,
2.8 MB for 84 000 edges), the fields (3.3 MB), the CPML (2.0), the coefficients (1.7), the lossy metal (1.4),
and cadlab's own command schemas (1.9 MB, in the 22 MB before the run). ems 0.0.17 (wave 32) holds the list as
runs of edges along z with the sequence of poles they share, 32 bytes a run, and a Debye edge's previous
field value as the field's `f32`. The Touchstone files of the sheet line, the `solid` line, the capacitor and
the via are 0.0.16's byte for byte; the process's peak on the development machine (four threads, three
runs each, 22 MB of it before the run) falls from 59 to 60 MB to 53 to 56 MB on the via and from 45 to 48 MB
to 43 MB on the `solid` line, and is within the scatter of the runs on the sheet line (42 to 43 → 40 to
43 MB) and the parts (35 → 32 to 35 MB). Of D98's two
items: `cb` and `db` on a mesh graded in the plane are a factor per axis times the edge's permittivity, but a
product formed in the loop is not the stored `f32` bit for bit, so they stay (24 bytes a node); a probe's
series is under 2 MB of these runs (eight to sixteen series of 4400 to 27 000 steps), and a running spectrum
would need the frequencies before the run: not taken. The two constants of a Debye pole per distinct edge
(16 bytes a pole, 5 MB here) are the pole's over the edge's `ε0·A/l`; formed in the loop they would not be the
same bits either.
**4. The lowest frequency, and what the end criterion costs.** Section 5's runs show the end criterion is the
largest error left in several rows, not only at 0.06 GHz: to −80 dB the capacitor is 0.0029 from the lines as
they are where the run to −50 dB reads 0.0064 (openEMS 0.0058), the 0.06 GHz points are 0.0007 dB from D52
where they read 0.0079 and 0.0090, the lossless lines' Σ\|S\|² end at 1.00000 and 1.00003 where they read
1.00066 and 1.00076. What a decibel costs depends on the region. The energy of a part's region goes on falling
as it fell (−50 dB at 5057 steps, −60 at 5569, −80 at 6657: 1.3 times the steps for 30 dB). A line's or the
via's has a knee just below −50 dB (the sheet line: −50 dB at 4257 steps, −55 at 5441, −60 at 7585, −70 at
15 617, −80 at 25 697; the via −50 at 23 265, −60 at 40 225, −70 at 87 361), which is where D105's 1.72 times
for −60 dB came from: after the knee a slow part decays by 10 dB in 0.9 to 2.8 ns, and it is that part the
0.06 GHz point lacks. *Extrapolating the tail, tried:* a matrix-pencil fit (sums of decaying exponentials,
order up to 12 and 20) on the last third, half or two thirds of each port signal of a run to −50 dB,
continued to infinity and added to the spectrum, against the same signal run to −80 dB. On the capacitor it
takes one probe from 3.8·10⁻³ of its spectrum at the worst of thirteen frequencies to 4·10⁻⁴ and leaves the
five others where they were or worse (0.11, 0.11, 0.19, 0.18, 0.18 before; 0.10, 0.10, 0.23, 0.19, 0.20
after), and with the longer windows up to seven times the spectrum; on the sheet line three probes are five
times worse on average (4·10⁻⁴ of the spectrum before, 2·10⁻³ after) and three a quarter better. The record
to −50 dB ends at the knee: the slow part is not yet what the window holds, and the fit has nothing to find
it by. Not taken. *A bounded continuation* (run on past the criterion while the energy still falls by
2 dB per twentieth of the steps so far, a quarter more steps at most) would take the parts to about −74 dB for
a quarter more steps and stop the lines within a twentieth, by the energies above. By the −80 dB values it
would move the capacitor's 0.0064 towards 0.0029 and the finer-mesh capacitor's 0.0094 towards 0.0095; it
was not run and is not made here: `end_criteria: 60dB` or more is there for who needs the low end, and its
cost per case is the list above.
**5. The seven figures of D105, step or end.** Every case on three steps (the Courant step through a switch in
the driver that is not kept, 0.0.15's and 0.0.16's), each to its own criterion and to −80 dB, where a run's
end no longer moves a printed digit. To −80 dB the three steps read: the sheet line's loss 0.0165 dB, RMS
3.1 %, mean +2.3 % on all three, its 0.06 GHz point 0.0064, 0.0065, 0.0065 dB; the `solid` line 0.0394,
0.0393, 0.0394 dB; the capacitor 0.0093 and 0.0029 on all three; the parts on the finer mesh 0.0073, 0.0095,
0.0078 on all three; the stripline 1.85 % and 0.450 % on all three; the lossless `solid` line 0.0163, 0.0162,
0.0164 dB; the lossless lines' Σ\|S\|² 0.99917 to 1.00000 and 0.99884 to 1.00002 or 1.00003. So six of D105's
seven moves (the capacitor's two, the capacitor with ESL and ESR, the stripline's εeff, the lossless `solid`
line's lowest point, and by the lines' evidence the bent line's Σ\|S\|², which was not run to −80 dB) were
where the run stopped: a longer step moves the 32-step grid of the criterion's checks, and the result with it
by one to three units of the fourth decimal in either direction. The seventh is the step: the 64 mm line's
worst \|S21\| against D52 reads 0.0896, 0.0898 and 0.0925 dB at 9 GHz to −80 dB on the three steps (0.0872,
0.0875, 0.0901 to its own criterion, which takes 0.0024 dB off each), growing with frequency (0.0005 dB at
6 GHz between 0.0.15 and 0.0.16) and present on `solid` copper over 64 mm only; the sheet line shows none.
Its cause was not found (the trapezoidal branches warp frequency by parts in 10⁵ at these steps, which is
not it).
**Time and memory beside openEMS.** On the development machine (AMD Threadripper 9970X, shared; the
comparison without openEMS; ems 0.0.15 → 0.0.17, the whole `sim.sparams` run): on one thread, one run each,
the sheet line 7.2 → 6.6 s, the `solid` line 16.4 → 13.4 s, the parts 3.9 and 4.0 → 3.1 and 3.0 s, the via
62.2 → 50.8 s; on four threads, the best of three, the `solid` line 4.7 → 4.5 s, the parts 1.17 and 1.16 →
1.07 s, the via 21.3 → 18.9 s, and the sheet line 2.0 → 2.0 s (three runs of each in turn: 2.06, 1.99, 2.08
and 1.97, 1.99, 2.05 s; its 7 % fewer steps are 0.1 s of time stepping, and the search for the shares takes
0.07 s of the compile where the single cells took 0.01). On the runner, beside openEMS in the same job (run
44114553606, `openems-comparison`, 46 minutes; an AMD EPYC 7763, four logical CPUs, the CPU model of D105's
run 44058840773, so its column is 0.0.15's on the same kind of machine, one run each):

| Case | Steps per port, ems 0.0.15 → 0.0.17 / openEMS | Time step, ems 0.0.15 → 0.0.17 / openEMS (ps) | Whole run on one thread, ems 0.0.15 → 0.0.17 / openEMS | Each at its fastest (threads), ems / openEMS | Peak memory, ems 0.0.15 → 0.0.17 / openEMS | Of it the run |
|---|---|---|---|---|---|---|
| `line_msl` | 4737 → 4385 / 8820 | 0.2544 → 0.2742 / 0.1982 | 15.9 → 14.8 / 48.4 s (3.3×) | 7.2 (4) / 48.4 (1) s (6.7×) | 45 → 42 / 75 MB | 22 → 19 / 24 MB |
| `line_lumped` | 11 425 → 8961 / 19 680 | 0.1049 → 0.1338 / 0.1011 | 39.2 → 31.6 / 141.7 s (4.5×) | 15.6 (4) / 130.9 (4) s (8.4×) | 50 → 46 / 78 MB | 27 → 23 / 27 MB |
| `via` | 32 961 → 27 041 / 27 216 | 0.0545 → 0.0664 / 0.0482 | 154.9 → 127.2 / 441.1 s (3.5×) | 74.3 (4) / 383.0 (4) s (5.2×) | 65 → 59 / 109 MB | 42 → 37 / 58 MB |
| `series_r` | 6689 → 5089 / 8037 | 0.1792 → 0.2356 / 0.1966 | 8.6 → 6.7 / 30.5 s (4.6×) | 3.7 (4) / 26.4 (4) s (7.2×) | 35 → 35 / 71 MB | 12 / 20 MB |
| `series_c` | 6689 → 5089 / 8460 | 0.1792 → 0.2356 / 0.1966 | 8.5 → 6.6 / 32.1 s (4.8×) | 3.7 (4) / 27.8 (4) s (7.6×) | 36 → 35 / 71 MB | 12 / 20 MB |
| `via_same` | 32 961 → 27 041 / 28 518 | 0.0545 → 0.0664 / 0.0460 | 154.6 → 126.8 / 300.1 s (2.4×) | 73.9 (4) / 281.1 (4) s (3.8×) | 66 → 57 / 92 MB | 44 → 35 / 41 MB |

ems is 2.4 to 4.8 times as fast on one thread and 3.8 to 8.4 times at each solver's fastest (0.0.15 on that
CPU: 2.0 to 3.8 and 3.2 to 6.6), takes fewer steps on every case (the via 27 041 for 27 216 and 28 518, where
0.0.15 took 32 961) on a longer step on every case, and the run's own memory is below openEMS's on every case
(the via on one mesh 35 for 41 MB, where it was 44; the `solid` line 23 for 27, where it was level). Accuracy
beside openEMS, the references being neither: the lines' loss from 0.5 GHz 0.0189 dB (4.2 % RMS) for 0.0268 dB
(15.8 %) and 0.0403 dB (5.9 %) for 0.0822 dB (17.0 %); Z₀ 0.3 for 4.0 % and 5.5 for 5.4 %; at 0.06 GHz 0.0079
and 0.0090 dB for openEMS's 0.0053 and 0.0051; the parts against ideal lines 0.0048 and 0.0100 for 0.0029 and
0.0089, against the lines as they are 0.0015 and 0.0064 for 0.0054 and 0.0058; the via 0.067 and, on one mesh,
0.112 dB from openEMS, as with 0.0.15. Both solvers' files are the same bytes on one and four threads in this
run. *Where openEMS is still ahead or level:* the 0.06 GHz point of both lines (its longer run; section 4);
the parts against ideal lines (0.0029 for 0.0048, 0.0089 for 0.0100) and the capacitor against the lines as
they are by 0.0006 (to −80 dB ems reads 0.0029 there); the `solid` line's Z₀ by 0.1 %. Not on time, steps,
time step or memory on any case any more.
**Tried and dropped.** Shares as the eigenvector of a power iteration gives them (each cell its part of
`A·v` over `λ·C·v`): exact where the vector is exact, and negative or without meaning where it is small, which
is most of a region. Shares balanced edge by edge with a multiplicative rule: 1.15 on a board mesh whose limit
is 1.195, where the shares per node and axis give 1.195. Sheet planes with free shares: the face in the plane
then costs both sides. An edge of lossy metal with its geometric quarter whatever the shares: the cell that
takes more has less of it by proportion and the closed form leaves the search nothing to go by. The walks of
the search as they come (every cell the least it needs): lopsided shares, and cells beside lossy metal that
read worse than with the halves; the middle of the walks from both ends is taken. The scheme's own stability
check after the bound has decided: it is the geometric bound over the grid, which a longer step does not pass,
and then a power iteration, 0.4 s of every run on these regions for a check the bound has made (0.0.16 pays
it, 0.0.17 does not).
**Verification.** ems: `scripts/check.sh` (fmt, clippy and the tests with all features and with none, docs,
`cargo +1.95 check`) before each of the three merges, four build jobs and four test threads, 14 minutes each:
the first stopped at the docs on four links to private items (the tests before it had passed; the links were
made text and the remaining steps run by hand, clippy with no default features among them), the second and
third printed ALL-OK. Release PRs 16 and 17: Test & lint on Linux, Windows and macOS, Docs and MSRV green, and
master's CI green, before each `gh pr merge`; 0.0.16 and 0.0.17 are on crates.io. The 400 000-step noise run
(`#[ignore]`) passed on 0.0.16's code, 12 minutes on one thread.
cadlab (Linux x86-64, ems 0.0.17 from crates.io, no patch): `cargo fmt --all --check`; `cargo clippy
--all-targets --all-features -- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`;
the test targets `fdtd`, `fdtd_ems`, `fdtd_parts`, `fdtd_conformal`, `openems`, `agent_session6`,
`agent_ergonomics`, `cli`, `mcp` and the `fdtd` unit tests in a debug build; in release the fourteen `#[ignore]`
validations of the table (on 0.0.15, on 0.0.16 and on 0.0.17, whose printed lines are 0.0.16's to every digit),
`long_runs_stay_bounded` and `solver_comparison` without openEMS (on 0.0.17's code before its release, by a
path dependency that is not committed). All pass; no golden file or snapshot changed (the export's mesh is
untouched). Not run here: the full `cargo test --all-features` (the merger's), `cargo deny`, cadlab's MSRV
check. With openEMS, on the branch `ci/ems-016` (the two openEMS jobs alone), run 44114553606
(https://github.com/KarpelesLab/cadlab/actions/runs/44114553606), ems 0.0.17 from crates.io: `openems`, the
nine oracle tests, passes in 10 minutes; `openems-comparison` passes in 46.
*Not established:* why the 64 mm `solid` line's loss at 9 GHz grows by 0.003 dB with the step; the bent
line's and the 45° lines' figures to −80 dB; the mesh change against openEMS; the CPML beyond frozen
coefficients and long runs; stability from noise on cadlab's own regions (cadlab's long runs are pulses);
the one-thread times on the development machine beyond one run each, on a shared machine; what a bounded
continuation of the run past its criterion reads (it was reasoned from the energies, not run).

### D108. guide 0.0.15 adopted: two nets that cross in a row of fine pitch have the crossing planned where the row's layer has no room; more runs that move routes, in other orders; the stubs that cadlab's fill makes needless taken out; the default unchanged (2026-11-18)
This round is for D106's gaps (guide's `docs/benchmarks.md`, "PCB routing of two nets that cross in a row").
`guide = { version = "0.0.15", default-features = false, features = ["pcb"] }`; nothing else in `Cargo.lock`
moves. Measured with the crate from crates.io, where D106's second release was measured from source.
**What guide 0.0.15 does:**
- *`RouteOptions::crossings`*, on by default in a run with a budget. In a row of fine pitch, the pad between two
  pads of another net, with a pad of its own net two along (the D+ and D- of a USB receptacle), is looked for a
  via straight out of its end with ways for the two pads beside it past the via. Where a track of the other net
  then fits round the via, nothing is planned. Where none does (nrfmicro: a diode's pad 1.9 mm in front of the
  row), the net's other pad gets a via of its own and a track on the other side of the board joins the two, the
  pads further along get the step aside that this needs, and all of it is fixed copper, as escapes are.
- *Hops taken out in cleanup*: a run of tracks between two vias that both go back to one layer is replaced by
  tracks on that layer where they fit within sixteen pitches, after the search for the whole net with dearer
  vias. A few vias in a hundred (lumenpnp-mobo 712 → 706).
**What the adapter does with it and beside it** (`router::guide`; ROUTER.md, "guide as a backend"):
- *`crossings` set by name.*
- *More runs that move routes* (`MOVE_AGAIN`, four): after D106's third run, while something is open and 3 s are
  left, another with the nets in another order, kept only if the board has less open. nrfmicro's run ended after
  20 s of 60 with one to three connections open; with them two orders of four have none and two have one.
- *The stubs that the fill makes needless* (`stub_pieces`): once the runs are over, with time for two fills, the
  pieces of new copper of a net with zones that are one via and one to four tracks are taken out, the board is
  filled again, those whose ends are then off their net's largest island are put back, and the result is kept if
  the board has no more open. guide plans on no pour of an outer layer and gives each such pad a stub to the
  plane inside; cadlab's fill joins most of them without.
**Measured** (ROUTER.md, "Native router and guide 0.0.15, D108"). 0.0.14 → 0.0.15 from source behind D106's
adapter, 60 s, four orders: 902 open on the twelve boards → 877 (nrfmicro 13 → 6, corne-cherry 87 → 79,
buspirate5-rev10 5 → 2, lumenpnp-mobo 1 → 0, cynthion 72 → 69, the others the same). With this adapter and the
crate from crates.io, four runs of each router in one hour: the twelve boards 195 to 238 open for guide (median
209) against 199 to 287 for the native router (224); the eleven without glasgow-revD1 72 to 87 (76) against 81 to
87 (84.5). Zero DRC errors other than unrouted, no via in a pad of its net, no call over 59.1 s. At 40 million
nodes, three orders, the nine boards a budget cuts: 797 → 791 (nrfmicro 10 → 2, lumenpnp-mobo 1 → 0,
tinytapeout-demo 36 → 35, corne-cherry 108 → 112, the others the same); a run that the budget stops is the same
from source and from crates.io. guide's curated boards: 1150 of 1151 connections with 712 vias, as before.
Vias, where the stubs go: lumenpnp-mobo 510, 571 and 568 in three orders for 699, 738 and 720 (706 in the fourth,
where taking them out left more open and they were put back), buspirate5-rev10 452 for 484, against the native
router's 344 and 313.
**One run of 48 had copper taken out by the verification**: buspirate5-rev10 in the order of seed 3, a via of GND
on a via of GND from a run after the first (`drc.hole_to_hole`; 15 items, counted as the one connection open
there). Not new: guide 0.0.13 does it in that order at 40 million nodes. It is in the gaps.
**Tried and not kept:**
- In guide (its `docs/benchmarks.md`): *every crossing planned*, those with room round the via too (at 60 s over
  six orders nrfmicro 11 for 20, but corne-cherry 154 for 137, tinytapeout-demo 67 for 54: the fixed copper is in
  the way of the pads beside it); *the crossing as routes that rip-up may take out* (nrfmicro 12 over four orders
  for 13: gone the first time a net crosses the place); *the negotiations for places made cheaper* (D106's first
  gap as it was put: a faster price once six or ten nets are left, 15 000 nodes for a net that is whole, 2.5 or
  4 million nodes a place, two places at most: corne-cherry 94 and 89 over four orders for 80 without, cynthion
  78 and 71 for 70); *the pours of outer layers planned on* (fewer vias, and tinytapeout-demo 153 open for 36 at
  40 million nodes, glasgow-revD1 837 for 454); *the parts of a board that its edge keeps apart told by a flood*,
  for sweep-v2.2's three connections: its outline is one area, so the edge is not what keeps its halves apart.
- In the adapter: *the runs after the first shortened to leave the time for the stubs* (2.5 times the first
  verification, an eighth of the budget at most, where 20 vias or more are of nets with pours): at 60 s over four
  orders corne-cherry 95 open for 79 and cynthion 77 for 69, for 18 vias on tinytapeout-demo and 45 on cynthion.
  *The stubs taken out before the runs that follow*: lumenpnp-mobo 697 vias for 702, as a run for a net with a
  plane gives every pad its stub again.
**Gaps, for guide's next release**, most useful first:
1. *corne-cherry's two QFN-56*: 15 to 23 open against 0 to 4, the one board behind in every run. What is open is
   pads of the two packages shut in by the tracks and vias of the pads beside them. Rows planned as routes are
   cut, as walls they were worse (D97), negotiations for places settle them in four minutes and not in one
   (D106, and above). What the native router does is plan a package's escapes together and keep them; what is
   not tried in guide is that plan as fixed copper for the pads that stay shut in after a first pass, as the
   crossing is here, in place of every pad before it.
2. *A via on a via of its net* in a run after the first (buspirate5-rev10, seed 3).
3. *The last connection behind nrfmicro's receptacle*: one in two orders of four, where the native router has
   none. The crossing is made; the nets beside it have too little room, and four more runs in other orders each
   end with the same one open.
4. *Vias*: a router that plans on the host's fill of outer pours has half of them. Told by the host after the
   fact, they go only where a run leaves the time; told by guide before, routes cut the pours. tinytapeout-demo
   has 450 to 602 against 270 to 281.
5. *Rule areas with a routed track width* (tinytapeout-demo's +3V3 pins: 0 to 16 open, most of the 16 of that
   net, against 3 to 6), *six layers in a minute* (glasgow-revD1: not profiled, this machine lets no process be
   sampled), *sweep-v2.2 at its deadline*: not touched, or tried and found not to be what was thought (above).
**Default: unchanged.** By the ranges of four runs of each in one hour, with 0.0.15: level on four boards
(buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, sweep-v2.2); ahead in every run on two (lumenpnp-mobo
0 against 3 to 17; cynthion 11 to 14 against 28 to 29); ranges that overlap on five (nrfmicro 0 to 1 against 0;
buspirate5-rev10 0 to 1 against 0 to 5; glasgow-revC3 31 to 35 against 31 to 36; glasgow-revD1 120 to 162 against
118 to 200; tinytapeout-demo 0 to 16 against 3 to 6, medians 8.5 and 4.5); behind in every run on one
(corne-cherry 15 to 23 against 0 to 4). The condition, at least as good on every board, is not met: not on
corne-cherry, not on nrfmicro by its median, nor shown on tinytapeout-demo. `router: guide` stays the second
opinion, and the native router stays.
*Why only the crossing with no room is planned:* fixed copper is a decision that no later net can undo. Where
rip-up finds the crossing itself it finds it with the pads beside it in mind, and where it never does, the plan
is the only way there is.
*Not measured:* the generated boards with 0.0.15 (a run with no budget plans no crossing; the curated boards are
as they were); more than four runs of either router; the taking out of stubs by a test of its own on a board
with a plane (`stub_pieces` has one; the rest is measured on the corpus); cadlab's oracles; KiCad's DRC on a
board guide routed; cadlab's whole test suite, which is run at the merge.

### D109. Thermal relief held end to end: pads in pours read from the filled copper, solder pads without relief and pads short of spokes reported by the DRC and the fab check, relief by default fitted to small pads, heat pads solid on purpose, `zone.relieve` (2026-11-18)
A pad soldered by hand or in an oven that sits solidly in a pour cannot be heated: cold joints on through-hole
pins, worst on inner planes, and tombstoned chip parts. Boards made by an agent driving another tool had no relief
on any ground pad; nothing told the agent. The fills had thermal reliefs since D46; nothing checked what a pad
ended up with.
**Audit, before.** A zone made with only a name, a net, layers and an outline was already `thermal`, gap = the
clearance, four spokes of max(track width, 0.25 mm), on every layer, and a through-hole pin got four spokes on
each inner plane; vias were always merged (measured: `tests/thermal_relief.rs`). What was wrong or missing:
- the spoke width took no account of the pad: on a 0.25 mm pin of a generated QFN-16 the four 0.25 mm spokes left
  copper on 69 % of the pad's outline;
- the footprint generator marked no pad: an exposed pad or a tab was relieved like a pin, which is wrong the
  other way (it is there to spread heat);
- a zone set to `solid` or `tht_thermal`, by a command or by an imported board, was kept without a word: on the
  twelve corpus boards as their projects route them, 664 of the 2376 pads in pours (pad and layer pairs) are
  solder pads joined solidly, 227 of them through-hole and 148 on inner layers (cynthion 373, glasgow-revC3 96,
  lumenpnp-mobo 109), 252 by a setting of the pad itself;
- no rule for spokes that do not survive: KiCad's `starved_thermal` was listed as a rule only KiCad has. A pad
  no spoke reaches was already an open connection (`drc.unrouted`); a pad on one spoke was nothing. On the
  corpus 143 relieved pads are short of two spokes that their own footprint leaves room for;
- nothing for copper that is no zone (a track wider than the pad, a copper drawing over it), for spokes as wide
  as the pad, for a gap or a spoke below what can be made, for the current of a power pin through its spokes;
- the outputs were right: the Gerber layers, IPC-2581 and the 2D render draw `copper_items`, fills included (an
  inner plane with reliefs is larger in outline than the solid one, tested); the KiCad export carries the zone's
  connection and the pads' own (`zone_connect`). The 3D view draws no copper at all.
**Reading the copper, not the settings** (`board::thermal`). In a band 10 µm wide around a pad's outline, per
layer: the pieces of zone copper (spokes that reach), the same with the net's tracks, vias and drawings (necks),
and the share of the outline covered. So a solid zone, spokes too wide, a pad on a drawing, a wide track and a
solid pad that other nets leave joined on one side all read as what they are. No relief = the pour on
`thermal_max_contact` percent of the outline (67), or other copper on as much when some of it is wider than the
pad; tracks no wider than the pad never make a pad solid (on small pads two of them take most of the outline:
17 BGA balls and chip pads of cynthion and glasgow-revD1 read solid by their tracks alone before the width was
asked for). Teardrop zones are tracks (corne-cherry has 921: taken as pours they put 295 more pads "in a pour",
253 of them on one neck). A pad is in a pour when zone copper within
its relief's reach goes on beyond it (the rim a fill leaves around a solid pad shut in by its own footprint is
the pad).
**What a pad is.** Through-hole pins and SMD pads are soldered. Heat pads are not held to the rules: a grid of
paste windows, or one window or none on a pad 2.5 times the footprint's median with no other pad as large beside
it, and same-number pads inside one. Found on the corpus: a paste window alone says nothing (an imported
board-to-board connector had one per pin, and `zone.relieve` made its 62 pads solid), and two equal lugs with one
number are solder pads. Not soldered: covered pads, pads without a number, non-plated holes, a one-pad footprint
that is a pasteless SMD pad or has a hole of 0.5 mm or less. Press-fit pins have no mark in the model and are
taken as through-hole pins.
**Rules** (warnings, in `drc.run`; BOARD.md has the table): `drc.solid_pad`, `drc.thermal_nominal`,
`drc.starved_thermal` (`min_thermal_spokes`, default 2; necks count, so a track into the pad is a second joint;
against what the pad's own footprint leaves room for, so a pin in a row is not reported for having its two ends),
`drc.thermal_size` (gap against the minimum clearance, spoke against `min_track_width`), `drc.current_spokes`
(IPC-2152 width against the spokes and tracks of a pad, all layers of a pin together). `fab.check` carries the
first three as `fab.*` warnings and checks the sizes against the profile's own minimum track and space
(`fab.thermal_size`): the fab-dependent numbers come from the profiles, none is a constant here.
*Warning, not error:* the board is connected and can be made; the other findings of that kind
(`drc.current_width`, `drc.split_crossing`) are warnings, `fab.export` refuses on errors only and fab errors
cannot be waived, and many imported boards are solid on purpose. An agent sees them in the counts of `drc.run`,
in `zone.fill`, at the import and in the fab check, each with the command that repairs.
**Solid on purpose** is said on the pad or the footprint (`zone_connection: solid`, quiet in every zone) or by a
waiver naming a zone or a net, which covers the `fab.` finding too. `zone.add --pads solid` still works and warns.
**Defaults changed.**
1. *Spokes fitted to the pad*: a zone that leaves `thermal_spoke` unset gives a pad spokes no wider than half
   its shorter side (whole micrometers, at least `min_track_width`); the shorter side is that of the rectangle
   with the pad's area and outline length, so it holds for turned and mirrored pads. A width set on the zone, the
   pad or the footprint is used as it is. No golden file moved (the list of commands apart): no test board had a
   relieved pad under 0.5 mm in a zone without a width. The KiCad import leaves a zone's width unset when it is
   cadlab's default, as it did: a file cannot say which was meant, and KiCad rewrites what cadlab exports (telling
   a board cadlab wrote by its generator failed `kicad_saved_board_imports_the_same` in CI). So such a zone of a
   board from KiCad is fitted too; on the corpus no pad reads differently for it.
2. *Generated exposed pads and tabs* carry `zone_connection: solid`.
3. *`min_thermal_spokes`* 2 and *`thermal_max_contact`* 67 in `board.rules`; KiCad's `min_resolved_spokes`
   raises the first and never lowers it (three corpus projects accept one spoke).
**`zone.relieve`**: zones that join pads solidly become `thermal`; `solid` settings of solder pads and their
footprints go (listed; `keep_own` keeps them); heat pads keep theirs or get one; pads that still read solid
through their spokes get a spoke of their own. One undo step. On the corpus (before → after, pad and layer pairs):

| | pads in pours | by necks 0/1/2/3/4+ | short of spokes | solid through-hole | solid SMD | on inner layers (solid) |
|---|---|---|---|---|---|---|
| twelve boards, as routed by their projects | 2376 → 2292 | 8/348/378/290/380 → 19/447/501/405/612 | 143 → 304 | 227 → 0 | 437 → 0 | 436 (148) → 400 (0) |
| cynthion | 575 → 519 | 0/56/31/27/75 → 1/91/100/92/222 | 1 → 62 | 180 → 0 | 193 → 0 | 169 (108) → 133 (0) |
| glasgow-revC3 | 170 → 142 | 0/6/9/2/16 → 5/16/13/15/52 | 0 → 19 | 46 → 0 | 50 → 0 | 93 (40) → 93 (0) |
| glasgow-revD1 | 361 → 361 | 7/108/92/72/33 → 5/151/106/51/41 | 80 → 114 | 0 → 0 | 42 → 0 | 20 (0) → 20 (0) |
| lumenpnp-mobo | 612 → 612 | 0/82/76/76/90 → 7/94/97/106/129 | 0 → 43 | 1 → 0 | 108 → 0 | 68 (0) → 68 (0) |
| buspirate5-rev10 | 235 → 235 | 0/43/66/47/13 → 0/41/79/63/14 | 36 → 39 | 0 → 0 | 28 → 0 | 30 (0) → 30 (0) |

No solder pad is left solid in a pour. What it costs is said by the same table: a layout made for solid pads has
no room for spokes everywhere, so 161 more pads are short of two (11 more with none), and 84 pad and layer pairs
are no longer in a pour at all, 36 of them pins of cynthion on its inner planes. The command lists each such pad
(`cut`, `zone.relieve_cut`) and each pad left starved; it cannot keep one of them solid alone, the pad settings
being per footprint. What stays as `drc.solid_pad` afterwards is copper that is no zone (lumenpnp-mobo 80: its
motor drivers' pads under wide tracks and drawings).
*Time, largest board* (glasgow-revD1, six layers, 1672 connections, release): the changes with the
fill and the reading they need 1.5 s, a fill 1.1 s, reading the pads and the five rules 0.64 s; `zone relieve`
from the command line 5.2 s with load and save; the import with its count 1.8 s. (`drc run` on that project takes
47 s from the command line, with a build of 14 November as with this one: not from this work, and not looked
into.)
**The routers** were measured, not changed. Each corpus board re-routed in 60 s, then the pads in pours counted
(`route_bench --corpus`, `CADLAB_BENCH_THERMALS=1`):

| twelve boards | pads in pours | by necks 0/1/2/3/4+ | short of spokes | solid solder pads | open connections |
|---|---|---|---|---|---|
| as routed by their projects | 2376 | 8/348/378/290/380 | 143 | 664 | (their own) |
| native router | 2346 | 70/318/330/251/420 | 219 | 655 | 280 |
| guide 0.0.14 | 2346 | 4/393/356/240/416 | 195 | 633 | 239 |
| native router, then `zone.relieve` | 2251 | 100/401/435/348/664 | 363 | 1 | |
| guide 0.0.14, then `zone.relieve` | 2249 | 8/437/496/341/661 | 290 | 1 | |

Short of spokes, projects' routing / native / guide: buspirate5-rev10 36 / 50 / 62, glasgow-revD1 80 / 117 / 69,
tinytapeout-demo 21 / 34 / 33, lumenpnp-mobo 0 / 3 / 21, sweep-v2.2 4 / 7 / 3, corne-cherry 0 / 4 / 5, cynthion
1 / 2 / 0, the other five 1 / 2 / 2 together. One run each, on one thread, on a machine with other work: the
order of size, not a ranking of the routers (D106 gives their ranges). The 61 pads with no neck of the native
router's glasgow-revD1 are connections it leaves open at 60 s (197). The one pad left solid after `zone.relieve`
is on glasgow-revD1 (`drc.thermal_nominal`; not looked into).

The solid pads are the zones' own: their number does not rise with either router. Both leave more pads short
of spokes than the projects' routing: a track or a via of
another net beside a pad takes the room of a spoke, and nothing in either router's cost says so. Left open.
guide needs no change for this work: its zones default to `thermal`, it is given cadlab's fills, and the pads'
own connection reaches it through `Zones::set_pad` (it is told the zone's spoke width, not the fitted one).
**Oracle.** `kicad_agrees_on_starved_thermals` (`tests/kicad_pcb_oracle.rs`): a header pin in a pour with a track
of another net around three sides, then two; KiCad's `starved_thermal` must report the first and not the second,
on the exported fill (cadlab's copper, no refill) and on its own refill, as `drc.starved_thermal` does. On the
corpus the two rules stay apart (each tool counts on its own fill; cadlab counts tracks as joints and asks what
the footprint leaves room for), and `drc.solid_pad` / `drc.thermal_nominal` have no counterpart.
It passes with KiCad 10 in CI (job "Oracle tests" of a run on the branch `ci/thermal-relief`), as do the other
oracle tests there; the corpus job of that run did not get as far as its tests (its KiCad installation step
failed), so the corpus comparison with these rules is not measured.
*Not done:* the routers (above); a setting per placed pad; the fitted spoke width in the KiCad export (KiCad's
refill of an exported board gives small pads the zone's width; the fill written is cadlab's) and in what guide
is told; a relief minimum of their own in the fab profiles (no profile has a field for one: the minimum track and
space stand in); IPC-2581 and the 2D render have no test of their own for the relief (they draw
the same copper items as the Gerber layers, which have one); a second spoke direction tried where none fits.

### D110. guide 0.0.16 adopted: no via beside a via of its net, the room of the spokes of pads in pours kept, what the board keeps apart found once and told; the default unchanged (2026-11-18)
This round is for D108's gaps and for the spokes of pads in pours, which D109 and D112 measure and neither
router kept (guide's `docs/benchmarks.md`, "PCB routing with room for spokes").
`guide = { version = "0.0.16", default-features = false, features = ["pcb"] }`; nothing else in `Cargo.lock`
moves. Measured with the crate from crates.io, on the fill of D112.
**What guide 0.0.16 does:**
- *No via beside a via of its net* (D108's failure, fixed in guide with a test that fails without it). A net
  that is completed is searched for from the copper it keeps, and what is found is added to it. In a
  negotiation that copper is out of guide's world for the search, so the place beside a via the net kept was
  free: two vias of GND 0.18 mm apart on buspirate5-rev10, one run in four in one order, which the verification
  took out. The net as it is completed now carries the vias of its copper, and its search keeps its own from
  them.
- *`RouteOptions::spoke_cost`*, off by default in guide, 4 here. A track of another net that ends where a spoke
  of a pad will be costs that many times its length more: a price and no wall, so a net with no other way
  takes it. Which spokes: those the host gives, `Zones::spokes`.
- *`Cause::Apart`*. In a run with a budget, the first time a search for a net fails the net is looked for on the
  board with none of guide's routes on it, on the lattice of the pass and then on the finer one where that
  ends within 4 million nodes. The connections that has no way for are counted; a net that has all its others
  has no more turns in the passes or the settling, and they are reported as apart. Not for a net with a plane.
**What the adapter does with it** (`router::guide`; ROUTER.md, "guide as a backend"; BOARD.md, "What a router
must be told"):
- *The spokes as the fill makes them.* Per pad of a net with a zone that holds it (teardrops are no zones), per
  layer of the zone that the pad is on, where `zones::connection_on` is thermal and `thermal::classes` says
  the pad is soldered: each spoke of `zones::Relief::of` (the gap, the width fitted to the pad, `dirs`), from
  the pad's anchor to the end of its rectangle (`zones::spokes`), as a `Spoke` of guide. A pad set solid on one
  layer has none there, a heat pad or a pad that is not soldered none at all. The fitted width also goes over
  as the pad's own spoke width.
- *`spoke_cost` 4*, set by name.
- *What the board keeps apart.* A connection with `Cause::Apart` from the first run reads `the board keeps
  <pad> apart from <pad>: with no other net routed, no track or via of net N gets from one to the other on
  any layer`, with the hint to join the two parts of the board or change what is between them, and no hint to
  the native router, which has no way either. Its net is in no later run. Told by the first run alone: in a
  later one the routes of the runs before are copper of the board.
- *A net with zones has its run on the filled board whatever the first run said of it.* A first run that ended
  on its own left such a net out of the second when its own search had found no way; cadlab's fill is not the
  fill guide planned on, and the time is there. sweep-v2.2: 3 open in four orders for 3, 4, 4 and 4.
**Not passed, of what BOARD.md lists:** the directions the fill falls back to (`Relief::alt`) and
`rules.min_thermal_spokes`. guide prices every spoke of `dirs` the same, whether the pad has two to spare or
none: a price that rises as a pad's free spokes fall below the minimum, counting the other four directions,
needs guide to keep count per pad while it routes, and is not built. Nor a connection by layer in guide's own
model of the zones (`PadConnection` is one a pad): guide fills nothing here, cadlab's fills go over as they
are, and the spokes are per layer.
**Measured** (ROUTER.md, "Native router and guide 0.0.16, D110"). With the crate from crates.io, four runs of
each router at 60 s: the twelve boards 196 to 205 open for guide (median 200) against 323 to 386 for the native
router (349); the eleven without glasgow-revD1 70 to 84 (78.5) against 81 to 105 (100). Zero DRC errors other
than unrouted, no via in a pad of its net, no call over 59.5 s, and no copper taken out by the verification in
any of the 48 runs. From 0.0.15 to 0.0.16 on the same fill, two orders: 238 and 218 open → 200 and 205. At 40
million nodes, three orders, the nine boards a budget cuts: 782 → 732 (corne-cherry 105 → 75, sweep-v2.2 16 → 9, glasgow-revD1 454 → 433, cynthion 56 → 53, tinytapeout-demo 33 → 35, buspirate5-rev10 12 → 16, nrfmicro 2 → 5, glasgow-revC3 104 → 106, lumenpnp-mobo 0 → 0); a run that the
budget stops is the same from source and from crates.io. guide's curated boards: 1150 of 1151 connections with
712 vias, as before (`spoke_cost` is off there).
**Spokes** (the same runs; `board::thermal::census`). Pads in pours short of two necks on the twelve boards:

| | no neck | short of two necks | open connections |
|---|---|---|---|
| as routed by their projects (D112) | 2 | 116 | (their own) |
| native router, four seeds | 36 to 49 | 154 to 170 | 323 to 386 |
| guide 0.0.15, two orders | 1 and 3 | 138 and 128 | 238 and 218 |
| guide 0.0.16, four orders | 2 | 82 to 100 | 196 to 205 |

Fewer than the projects' own routing leaves, with fewer connections open than before: kept. Where they are
left: buspirate5-rev10 26 to 35 and glasgow-revD1 38 to 41, whose pads in pours are close together, then
lumenpnp-mobo 5 to 13 and tinytapeout-demo 6 to 11. Before the fill of D112, with the spokes worked out by guide
from the zones and the pads' settings (four spokes along the axes, the zone's width), two orders had 127 and 127
for 195 and 169 with 0.0.15: the gain is of the same size either way, and the host's spokes are the ones that
are there.
**Tried and not kept:**
- In guide (its `docs/benchmarks.md`): *a via beside the net's own hole refused in guide's world* (the first
  fix for the via on a via; six orders at 40 million nodes: nrfmicro 22 open for 1, corne-cherry 299 for 231,
  buspirate5-rev10 40 for 15: a negotiation routes a net across its own old route); *a price on vias in the
  room of a spoke too* (lumenpnp-mobo 3 open in one order of four, where it had none in any run before, for no
  fewer pads short); *the price at 2 and at 12* (135 and 129 pads short at 2, and at 12 no fewer than at 4 with
  289 and 305 open for 255 and 212); *ways out that stay for the pads of a row that the routes leave shut in*,
  D108's first gap as it was put: a straight track out of an end of the pad to a via as fixed copper, in the
  runs that move routes, at the place in the way of the fewest routes (corne-cherry 102 open over four orders
  for 89 at 60 s, 84 for 84 at 120 s) or only where none is in the way (90 for 89); *more rounds of cleanup and
  a shorter way for each net*, the others where they are (lumenpnp-mobo 657 and 680 vias for 706 and 510, in
  27 s of cleanup for 5); *dearer vias* (2 and 3 mm of track for 1: 10 to 25 % fewer vias at 40 million nodes,
  with buspirate5-rev10 28 and 19 open for 12 and glasgow-revD1 602 and 601 for 440); *a pass of rip-up given
  four times as long before it ends* and *a net ripped up eight times before it is locked* (232 open for 222 on
  five boards; buspirate5-rev10 33 for 12); *nets with a plane looked for on the bare board too* (sweep-v2.2 4
  and 5 open for 3); *a net that goes on at the width its tracks have* in a later run (tinytapeout-demo 30 open
  over four orders for 30).
- In the adapter: *a via at 2 mm of track on boards of two layers* (at 60 s over four orders corne-cherry 75
  open for 89, nrfmicro 5 for 5, sweep-v2.2 16 for 15: within their spread, and sweep-v2.2 5 and 5 with it
  beside the rest of this round, for 3 and 3).
**What the native router does differently on corne-cherry**, looked at pad by pad on its first QFN-56
(`CADLAB_BENCH_DUMP=U1`, `CADLAB_BENCH_FANOUT=only`): it plans no escape for that part at all, whose pads are
on its grid, and for the second only straight stubs with no via out of the two rows that are off the grid. Of
the 56 pads it leaves 36 outwards and 16 inwards to a via between the row and the exposed pad, at the depths
its grid has; guide leaves 33 outwards and 9 inwards, and 9 have nothing: nets that were routed, taken out by
another, and found no way back past nets that are locked by then. So D108's first gap is not a plan that guide
lacks. The native router routes every net across every other at a price, on one grid, until none is in
another's way; guide routes a net at a time and takes others out, and its negotiations are local and cost
four minutes to settle those pads (D106).
**Gaps, for guide's next release**, most useful first:
1. *corne-cherry's two QFN-56*: 15 to 21 open against 0 to 4, still the one board behind in every run. What it
   needs is a negotiation over all the nets of a part at once that a minute pays for: congestion priced on a
   grid, with exact geometry only for what is kept. Fixed ways out for the pads left shut in were built and
   measured (above), and are not it.
2. *nrfmicro's last connection*: 0 to 2 against 0. It is one of the two data nets from the receptacle to the
   module, across the board, with the nets beside it settled in its way and not shut in anywhere: the same
   one-net-at-a-time limit.
3. *Vias*: lumenpnp-mobo 511 to 686 against 309 to 343, tinytapeout-demo 473 to 691 against 238 to 290. Half
   of lumenpnp-mobo's are its other nets' and not stubs to planes (some 400 of 706 with 0.0.15); routing a net
   again with dearer vias, the others where they are, removes few, and dearer vias while routing cost
   connections on the boards of four and six layers.
4. *Six layers in a minute* (glasgow-revD1): timed by net with counters in guide's passes, since this machine
   lets no process be sampled: of 42 s of its first pass at 40 million nodes, 27 s are searches, none over
   2.4 s, and 15 s are outside any search, in what a turn does before and after it. That third is where to
   look.
5. *The spokes that are still lost*: every spoke costs the same. The price that BOARD.md describes, of a neck
   only while fewer than `min_thermal_spokes` remain among the eight directions, needs a count per pad kept
   while routing.
6. *Rule areas with a routed track width*: not built. tinytapeout-demo's +3V3 pins, which D108 gave it for, are
   joined on the fill of D112 (2 to 10 open against 2 to 15).
7. *sweep-v2.2's time*: its two nets that the board keeps apart are found after one search each, and the run
   still ends at its deadline for the one connection of its ground that has no way, which is not looked for
   on the bare board (a net with a plane counted there came out with more open than it has).
**Default: unchanged.** By the ranges of four runs of each, with 0.0.16: level on four boards
(buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, sweep-v2.2); ahead in every run on three
(lumenpnp-mobo, cynthion, glasgow-revD1); ranges that overlap on four (buspirate5-rev10, tinytapeout-demo,
glasgow-revC3, and nrfmicro at 0 to 2 against 0); behind in every run on one (corne-cherry). The condition, at
least as good on every board, is not met: not on corne-cherry, and not on nrfmicro by its median. `router:
guide` stays the second opinion, and the native router stays.
*Not measured:* the generated boards with 0.0.16; more than four runs of either router; the native router and
guide in the same hour (the native router's four runs came first, alone); KiCad's DRC on a board guide routed;
cadlab's oracles; cadlab's whole test suite, which is run at the merge.

### D111. ems 0.0.18: the loss that moved with the time step was the absorbing layers' convolution, first order in the step, and is second order now; a run goes on past its energy criterion while the ports' signals still add to the lowest frequency (2026-11-18)
D107 left two things open: the 64 mm `solid` line's worst \|S21\| against D52, which read 0.0896, 0.0898 and
0.0925 dB at 9 GHz on three time steps, cause not found; and the end of a run, which was the largest error
left at the lowest frequency and on the parts. This is what each came to, and what it cost. No openEMS, CSXCAD
or other GPL code was read.
**1. The drift with the step: the CPML's convolution.** A study in the step on one mesh (the 64 mm line of
`sim_sparams_microstrip_validation`, every run to −80 dB so that its end moves nothing; the step through
switches in the driver that are not kept), \|S21\| at 9.01 GHz:

| Model | 0.1338 ps (0.0.17's step) | 0.0994 ps (Courant) | 0.0701 ps | 0.0497 ps | Per ps |
|---|---|---|---|---|---|
| lossy laminate, lossy `solid` copper | −1.74947 dB | −1.74664 | −1.74414 | −1.74182 | 0.082, 0.085, 0.114 dB |
| lossy laminate, perfect copper | −1.62102 (0.1347 ps) | −1.61818 | | | 0.080 |
| lossless laminate, lossy copper | −0.16636 | −0.16350 | | −0.15940 | 0.083, 0.082 |
| lossless laminate, perfect copper | −0.03797 (0.1347 ps) | −0.03506 | | −0.03100 | 0.082, 0.082 |
| the same, twelve cells of layer for six | −0.02447 | −0.02442 | | | 0.001 |
| lossy laminate, lossy sheets (the step 0.2741 and 0.0975 ps) | −1.77520 | −1.76328 | | | 0.067 |

The move is linear in the step, not quadratic, it is the same 0.082 dB/ps with the laminate's loss, with the
copper's, with both and with neither, and it goes away with a thicker layer. So none of D107's candidates:
not the lossy metal's update, not the Debye poles, not dispersion (the cells along the line are 0.5 mm and the
step is set by cells of 35 µm: the time term of the dispersion relation is 10⁻⁶), not the ports. A lossless
line has no business losing 0.038 dB; what it loses is what the layers 5 mm beside it take from its near
field, and that depended on the step. The cause is in the layer's recursion (Roden and Gedney's, which ems
had): it takes the field derivative constant over a time step, at its value at the step's end. As a filter of
the derivative's samples that is the layer's `−c/(jω + p)` times `1 + jω Δt/2`, first order in the step, and
for σ ≫ ω ε₀ it is the layer of a conductivity σ/(1 − σ Δt/(2ε₀)): in the outer cells of a layer whose cells
set the step, where σ Δt/ε₀ reaches 1.8, several times the conductivity asked for, and more with a longer
step. ems 0.0.18 (wave 33 of its design notes; `CpmlConvolution::Linear`, its default) takes the derivative
linear over the step, the piecewise-linear recursive convolution of Kelley and Luebbers applied to the
layer, `ψⁿ = b ψⁿ⁻¹ + a₀ Dⁿ + a₁ Dⁿ⁻¹`: second order, and by a change of the state's variable the same
recursion with other coefficients, so the loops, the memory and the time of a step are what they were. The
lossless line reads 0.02777, 0.02776 and 0.02776 dB at the three steps (the old rule's own line to a step of
zero gives 0.0269), and the lossy one, worst point against D52 to −80 dB, 0.0820, 0.0822 and 0.0816 dB at
0.1338, 0.0994 and 0.0497 ps where it read 0.0924 at the first: nearer D52 at the long step than it was at
the Courant step (0.0896). *Stability:* the frozen layer's stretch still maps \|z\| ≥ 1 into a disc on a
diameter within [0, 1/κ], and its characteristic polynomial has no root outside the unit circle up to the
Courant limit of the unstretched cell over κ 1 to 15, σ Δt/ε₀ 0 to 100 and α Δt/ε₀ 0 to 5, for either rule
(ems's unit test scans both); ems's `tests/step_limit.rs` steps boards from noise at factor 1 for 30 000
steps with the layer in seven variants (κ 5, no shift, 2.5 and 8 times the conductivity, eight cells with κ 3,
and the old rule twice), nothing above its start, and every other test of that file runs on the new rule;
`long_runs_stay_bounded` below. No energy proof, as before (D105).
*What it changes besides.* The old rule's larger conductivity absorbed waves near grazing and guides just
above their cutoff better than the conductivity asked for does (ems's notes have the figures: a slab at 30°
in a Bloch cell 2.8·10⁻⁴ off the power balance for 5.7·10⁻⁴ now, a parallel-plate guide driven across its
cutoff −78 dB for −50 dB, both regained with `sigma_factor` 1.5 and 2), and absorbed normal incidence worse
(−83.6 for −85.8 dB; a point source in 3D −73 for −85 dB). On the 64 mm line 1.5 and 2 times the
conductivity read 0.0861 and 0.0745 dB where the usual reads 0.0820: not monotone, so it is not tuned here,
and cadlab's layer keeps ems's conductivity. Whether the step should be capped by an accuracy criterion at
the highest frequency: no, there is nothing of second order left to cap in what was measured (the three
steps agree to 0.0006 dB of 1.74).
**2. The end of a run.** What a spectrum at the lowest frequency lacks is the signals after the run's end,
and D107 measured that the energy has a knee just below −50 dB on the lines, after which a slow part falls
10 dB in 0.9 to 2.8 ns: the low end of the pulse in layers whose frequency shift lies above it. Three rules
were built in ems and run on the oracle's cases (ems's `EndCriteria::settle`, in the engine's loop, where the
energy criterion is; each asks, from the first check that meets the energy criterion on, something of every
voltage and current probe's running spectrum `X` at the lowest frequency of the result, relative to the sum
`A` of the signal's absolute values):
- *the spectrum's change over the last quarter of the run below a tolerance.* A slow tail moves the spectrum
  little per window and has much to come: with 10⁻⁴ the lines ended at 1.44 and 1.37 times the steps, 0.0035
  and 0.0053 dB from D52 at 0.06 GHz, and the parts ran 1.2 to 1.7 times for nothing, the window having to
  clear the pulse (the resistor, done at its 5089 steps, ran 6177). Dropped.
- *the tail by the ratio of two windows' changes* (a geometric series). A spectrum's increments are not
  monotone; on the recorded signals the rule ended a line anywhere between 1.12 and 5 times. Dropped, not run
  in the solver.
- *the tail by the energy's rate, taken.* A signal that falls as the fields do falls at half the rate of
  their energy, so with `W₀`, `X₀` the energy and the spectrum an eighth of the run earlier, the spectrum
  still lacks about `2 |X − X₀| / ln(W₀/W)`; the run ends when that is at most 10⁻⁴ of `A` for every probe,
  or after `max_extension` times the steps the energy criterion took. This is D107's "keep going while it
  still falls" turned round: a field that falls fast has little to come and ends at once, one that falls
  slowly is the one to wait for.

| Case (asked for) | Energy alone: steps, at 0.06 GHz | With the test, no cap | Cap of a half (ems's own) | Cap of one (cadlab's) |
|---|---|---|---|---|
| lossy line, sheets (50 dB) | 4385, 0.0079 dB off D52 | 10 209 (2.33×), 0.0001 | 6593 (1.50×), 0.0031 | 8801 (2.01×), 0.0008 |
| lossy line, `solid` (50 dB) | 8961, 0.0090 | 20 961 (2.34×), 0.0002 | 13 473 (1.50×), 0.0040 | 17 953 (2.00×), 0.0012 |
| series 1 pF (50 dB) | 5089; 0.0100 and 0.0064 from circuit theory | 6881 (1.35×), ends at −81 dB; 0.0093 and 0.0029 | the same | the same |
| series 50 Ω (50 dB) | 5089; 0.0048 and 0.0015 | 5153 (1.01×); the same | the same | the same |
| via transition (40 dB) | 27 041 | 28 577 (1.06×) | the same | the same |
| the sheet line asked for 60 dB | 7585, 0.0018 | 10 209 (1.35×), 0.0001: the same end | the same | the same |
| 64 mm line (60 dB, the default) | 15 713 | 19 009 (1.21×) | the same | the same |

For comparison, the energy criterion alone needs −80 dB, 5.9 times the steps, to read 0.0007 dB at 0.06 GHz
on the lines, and there the test's own end reads 0.0001 and 0.0002 at 2.33 times. *What "modest" came to:*
at `sim.sparams`'s default of 60 dB the test ends every case measured, 1.0 to 1.35 times the steps of the
energy criterion. Asked for 50 dB, as the oracle's cases are, the lossy lines hit the cap (the line with
perfect copper on the Debye laminate, asked for 50 dB too, ends by the test at 1.80 times, 16 001 steps). With ems's own cap of a half the first comparison
run (44159134055) had the `solid` line at 0.0040 dB for an openEMS that had run 22 221 steps and read 0.0035
(it read 0.0051 on 19 680 steps in D107's run): not nearer. cadlab therefore sets the cap at one (`SETTLE_MOST`),
twice the steps at most, which the two lines take and nothing else does: 0.0008 and 0.0011 dB. `settle: false`
on `sim.sparams` is the energy criterion alone, the Touchstone data of ems 0.0.17's criterion on the new
layer. The tolerance is of the signal's size, not of the spectrum at that frequency (a capacitor's current at
its lowest frequency is next to nothing, and a tolerance of it would never be met), so a frequency the pulse
hardly excites is held less tightly; the bent line below is where that shows.
**3. The `solid` line's Z₀** (5.5 % from the field solver for openEMS's 5.4 %): diagnosed, not changed. The
figure is the largest to 2.6 GHz of the Z₀ of the ABCD matrix between lumped ports at the track's ends, and
it is the last point that makes it: 49.29, 49.53, 49.90, 50.40, 51.22, 52.77 Ω from 0.06 to 2.54 GHz against
49.8 to 50.0, then 57.6 Ω at 3.03 GHz and nonsense at the half-wave resonance at 3.5 GHz. At 2.54 GHz the
line is 132° long, where √(B/C) divides by a sine that is going to zero and the ports' own reactance, which
nothing de-embeds, is what is left: both solvers read the same 5.4 to 5.5 % there, 0.1 % apart by their
ports. Below 1.5 GHz ems is within 1.1 %. The same copper between microstrip ports with reference planes
(the 64 mm line) is within 2.94 %, the sheet line within 0.3 %. A figure for the line would need the limit
at a shorter electrical length or ports with planes; the test's is the oracle's and is left.
**Validation: every case against its reference, 0.0.17 and 0.0.18.** One harness, two worktrees (0.0.17 at
3bfce1a), ems from crates.io in both, release, four threads. *What moved the wrong way, first:*
- the 64 mm line's point at 0.595 GHz, 0.0010 → 0.0039 dB from D52 (−0.7 → −2.9 %): the run to −80 dB has it
  at 0.0033 with either layer, so 0.0010 was where the shorter run happened to stop; the largest to 2.5 GHz
  is 0.0042 → 0.0039;
- the `solid` line's mean loss from 0.5 GHz, +4.7 → +5.3 % of D52 (its largest 0.0403 → 0.0397 dB, RMS 5.9 →
  5.8 %): the longer run's value, as D107's run to −80 dB read (+5.1 %);
- the stripline's Z₀, 1.78 → 1.86 % from the field solver: the value D107 found on every step to −80 dB (1.85);
- the capacitor on the finer mesh, 0.0094 → 0.0095 from circuit theory: likewise (0.0095 to −80 dB);
- the lossless line next to the absorber, Σ\|S\|² from 0.99908 to 0.99901 at its low end (1.00066 → 1.00035 at
  the high end);
- the bent line's largest Σ\|S\|² with perfect copper and conformal cells, 1.0003, 1.0001, 1.0001, 1.0001 →
  1.0005, 1.0006, 1.0005, 1.0005, and three of the six lossy models by 0.0002 to 0.0005 (two down, one up).
  This is the end of the run, not the layer: with `settle` off the twelve figures are 0.0.17's to every
  digit. The runs are asked for 70 dB and go on 0 to 4 % further; the largest value is at the lowest
  frequency, where the pulse has little and the tolerance of section 2 is loosest.
Each is inside its test's tolerance.

| Case | Reference | 0.0.17 | 0.0.18 |
|---|---|---|---|
| lossy line, sheets (`line_msl`): loss from 0.5 GHz | D52 | within 0.0189 dB, RMS 4.2 %, mean +2.7 % | 0.0167 dB, 3.1 %, +2.5 % |
| the same at 0.06 GHz; Z₀ | D52 (0.0072 dB); 2D field solver | 0.0079 dB off; 0.3 % | 0.0008 dB off; 0.3 % |
| lossy line, `solid` (`line_lumped`): loss from 0.5 GHz | D52 | 0.0403 dB, RMS 5.9 %, mean +4.7 % | 0.0397 dB, 5.8 %, +5.3 % |
| the same at 0.06 GHz; Z₀ | D52 (0.0086 dB); field solver | 0.0090 dB off; 5.5 % | 0.0012 dB off; 5.5 % |
| series 50 Ω (comparison) | circuit theory: ideal lines; the lines as the field solver has them | 0.0048; 0.0015 | 0.0048; 0.0015 |
| series 1 pF (comparison) | the same two | 0.0100; 0.0064 | 0.0093; 0.0029 |
| 64 mm lossy microstrip, \|S21\| | D52 | −1.4 to +5.4 %, RMS 2.78 %, worst 0.0900 dB; to 2.5 GHz 0.0042 dB | −2.9 to +4.9 %, RMS 2.65 %, worst 0.0820 dB; to 2.5 GHz 0.0039 dB |
| the same, Z₀ and εeff | 2D field solver | within 2.94 % and 0.98 % | 2.94 % and 0.98 % |
| lossless line next to the absorber, Σ\|S\|² | passivity (1) | 0.99908 to 1.00066 | 0.99901 to 1.00035 |
| solder mask: Z₀, εeff; bare εeff | 2D field solver (50.07 Ω, 3.1623; 3.0709) | 50.62 Ω, 3.1542; 3.0837 | 50.61 Ω, 3.1542; 3.0836 |
| parts on the finer mesh: 50 Ω, 1 pF, 1 pF with ESL and ESR | circuit theory | 0.0073, 0.0094, 0.0078 | 0.0073, 0.0095, 0.0078 |
| the parts of `measure_parts`: R, C, RLC, 0.5 to 4.5 GHz | circuit theory | 0.0048, 0.0100, 0.0077 | 0.0048, 0.0093, 0.0077 |
| cavities: PEC modes, Q from σ, from copper plates, Debye filling | analytic modes and Q | −0.08 %; +0.04 %; +0.08, −0.14, −0.22 %; resonance −0.01 %, Q −0.65 to −0.74 % | the same lines (no layer, no ports) |
| stripline: Z₀, εeff | 2D field solver | within 1.78 %, 0.46 % | 1.86 %, 0.45 % |
| lossless-copper `solid` line with the Debye laminate, from 0.5 GHz; at 0.06 GHz | D52 | within 0.0201 dB; 0.0121 dB off | 0.0166 dB; 0.0013 dB off |
| lossless sheet line (`measure_microstrip`): Z₀, εeff, Σ\|S\|² | 2D field solver; passivity | 1.22 %, 0.59 %; 0.99843 to 1.00076 | 1.20 %, 0.57 %; 0.99884 to 0.99999 |
| routed 45° line, six meshes, and the 45° line of `measure_diagonal`: \|S21\| at 10 GHz of a lossless line | 0 dB | −0.07, −0.18, −0.11, −0.12, −0.10, −0.04 dB; `solid` −0.11 | −0.03, −0.17, −0.08, −0.12, −0.07, −0.03 dB; `solid` −0.07; S11 within 1.6 dB and the phases within 0.4° of what they were |
| via on perfect sheets (`measure_via_convergence`) at 4 GHz | the finer meshes | −0.425 dB staircase, −0.376 dB conformal | −0.426, −0.377 dB |
| bent line, six copper models on two meshes: largest Σ\|S\|² | passivity | 1.0003, 1.0001, 0.9979, 0.9978, 1.0001, 0.9983; 1.0001, 1.0006, 0.9977, 0.9979, 1.0001, 0.9983 | 1.0005, 1.0001, 0.9977, 0.9983, 1.0006, 0.9979; 1.0005, 1.0006, 0.9978, 0.9980, 1.0005, 0.9979 |
| session 6's RF path; the removed engine's recorded figures (`RECORDED_*`, `ems_runs_the_region`) | its ideal chain (not a reference); cadlab's own engine at 3aa423f | the tests pass | pass, no tolerance or constant touched |

**Beside openEMS.** Run 44174491916 (https://github.com/KarpelesLab/cadlab/actions/runs/44174491916, branch
`ci/ems-018`, the two openEMS jobs alone; `openems-comparison` 27 minutes, `openems`, the nine oracle tests,
13), ems 0.0.18 from crates.io, openEMS-Project 1c6e5c1, one run each, on an AMD EPYC 9V45 with four logical
CPUs. That is not D107's CPU (an EPYC 7763): the runners hand out three kinds, and openEMS's speed beside
ems's differs between them more than between ems's versions (with 0.0.12 the 9V45 read 1.4 to 2.4 times on one
thread where the 7763 read 1.8 to 2.9), so the time columns are not D107's continued.

| Case | Steps per port, ems 0.0.17 → 0.0.18 / openEMS | Time step, ems / openEMS (ps) | Whole run on one thread, ems / openEMS | Each at its fastest (threads), ems / openEMS | Peak memory, ems / openEMS | Of it the run |
|---|---|---|---|---|---|---|
| `line_msl` | 4385 → 8801 / 8820 | 0.2742 / 0.1982 | 16.2 / 20.9 s (1.3×) | 7.8 (4) / 20.9 (1) s (2.7×) | 43 / 75 MB | 20 / 24 MB |
| `line_lumped` | 8961 → 17 953 / 22 221 | 0.1338 / 0.1011 | 31.6 / 68.9 s (2.2×) | 16.4 (4) / 68.9 (1) s (4.2×) | 48 / 78 MB | 25 / 27 MB |
| `via` | 27 041 → 28 577 / 27 216 | 0.0664 / 0.0482 | 75.5 / 252.4 s (3.3×) | 42.5 (4) / 252.4 (1) s (5.9×) | 61 / 109 MB | 38 / 58 MB |
| `series_r` | 5089 → 5153 / 8037 | 0.2356 / 0.1966 | 3.9 / 12.4 s (3.2×) | 2.1 (4) / 12.4 (1) s (5.9×) | 36 / 71 MB | 12 / 20 MB |
| `series_c` | 5089 → 6881 / 8460 | 0.2356 / 0.1966 | 5.0 / 12.9 s (2.6×) | 2.7 (4) / 12.9 (1) s (4.7×) | 36 / 72 MB | 13 / 20 MB |
| `via_same` | 27 041 → 28 577 / 28 518 | 0.0664 / 0.0460 | 70.0 / 131.5 s (1.9×) | 39.9 (4) / 131.5 (1) s (3.3×) | 62 / 91 MB | 38 / 40 MB |

The run before it (44159134055, the cap of a half, on an Intel Xeon 6975P-C): 15.2 / 32.2 s, 32.8 / 95.9,
105.2 / 250.7, 5.1 / 17.1, 6.9 / 18.2 and 109.2 / 183.1 s on one thread (2.1, 2.9, 2.4, 3.4, 2.7 and 1.7
times) and 4.3, 6.1, 3.9, 5.0, 4.0 and 3.0 times at each solver's fastest, the lines on 6593 and 13 473
steps. ems is 1.3 to 3.3 times as fast on one thread and 2.7 to 5.9 times at each solver's fastest on the
9V45, where openEMS was fastest on one thread on every case. The settle test costs the lines their lead in
steps (8801 for 8820, where 0.0.17 took 4385) and about half their lead in time; the process's peak is 1 to
5 MB above 0.0.17's in D107's run (the probes' series are up to twice as long), and the run's own memory is
below openEMS's on every case as before.
Accuracy, the references being neither solver (ems / openEMS): the loss at 0.06 GHz 0.0008 / 0.0053 dB on the
sheet line and 0.0011 / 0.0035 on the `solid` line (0.0.17: 0.0079 and 0.0090); from 0.5 GHz 0.0167 dB,
3.1 % RMS / 0.0268 dB, 15.8 % and 0.0397 dB, 5.8 % / 0.0862 dB, 17.0 %; Z₀ 0.3 / 4.0 % and 5.5 / 5.4 %; the
resistor 0.0048 / 0.0029 from ideal lines and 0.0015 / 0.0054 from the lines as they are; the capacitor
0.0093 / 0.0089 and 0.0029 / 0.0058 (0.0.17: 0.0100 and 0.0064); the via 0.066 dB from openEMS, on one mesh
0.039 (0.111 in the run before: openEMS's own result moves with its machine, 28 518 and 29 876 steps on its two
ports this time). Both solvers' files are the same bytes on one and four threads but openEMS's `via_same`.
*Where openEMS is still ahead or level:* the parts against ideal 50 Ω lines (0.0029 for 0.0048 and 0.0089
for 0.0093), which is the reference that takes the board's 50.8 Ω lines for 50 Ω, and against the lines as
they are ems is nearer by 0.004 and 0.003; the `solid` line's Z₀ by 0.1 % (section 3); and the number of
steps on the via, 27 216 and 28 518 for ems's 28 577 on a step 1.38 and 1.44 times as long, where 0.0.17 took
27 041. Not the 0.06 GHz points and not the capacitor against the lines as they are any more; not on time,
time step or memory on any case.
**Also.** `solver_comparison` took one thread and every core of the machine when `CADLAB_CMP_THREADS` did
not say: on a workstation that is 64 threads of other people's. It takes one and at most four now
(`CMP_MOST_THREADS`), which is the runners' count, so CI's table is as it was.
**Tried and dropped** (beyond the two end rules above): a larger conductivity for cadlab's layer (1.5 and 2
times read 0.0861 and 0.0745 dB on the 64 mm line for 0.0820: not monotone, and one case is no ground for a
tuning); ems's own cap of a half for cadlab (the `solid` line's 0.06 GHz point 0.0040 for openEMS's 0.0035
in one run); the tolerance relative to the spectrum at the frequency and not to the signal's size (a
capacitor's current there is next to nothing: the test would run every part to the cap).
**Verification.** ems: `scripts/check.sh` (fmt, clippy and the tests with all features and with none, docs,
`cargo +1.95 check`), four build jobs and four test threads, 15 minutes: stopped once at clippy on a type
(named, run again), then ALL-OK. Two of its tests were changed with the layer and say why: the Bloch slab at
30° and the plates into the CPML take 1.5 and 2 times the conductivity, which is what they had by the old
rule's error. Master's CI green on Linux, Windows and macOS, docs and MSRV; release PR 18 the same five green
before `gh pr merge`; 0.0.18 is on crates.io.
cadlab (Linux x86-64, ems 0.0.18 from crates.io, no patch): `cargo fmt --all --check`; `cargo clippy
--all-targets --all-features -- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`;
the test targets `fdtd`, `fdtd_ems`, `fdtd_parts`, `fdtd_conformal`, `openems`, `agent_session6`,
`agent_ergonomics`, `cli`, `mcp` and the `fdtd` unit tests (six, the one that holds `SOLVER` to `Cargo.toml`
among them) in a debug build; in release the thirteen `#[ignore]` validations of the table and the six
comparison cases without openEMS, on 0.0.17 and on 0.0.18, and again after the cap went from a half to one
(only the two lines and the lossless-copper line moved); `long_runs_stay_bounded` on the crates.io crate (the
five cases to their end and then ten times as many steps: the lines −72 dB at twice their 8801 and 17 953
steps and −90 and −96 dB at the end, the via −64.5 and −85.7 dB after 285 761 steps, the parts −128 dB;
nothing came back) and `solver_comparison` without openEMS on the crates.io crate with `CADLAB_CMP_THREADS=1,4`
(D107 had run both on a path dependency). All pass. `agent_ergonomics` failed once on the way: the new
option's help text took `sim.sparams`'s schema 77 bytes past the 11 000 that test holds it to, and the text
was shortened. No golden file or snapshot changed. Before the release the same validations were run on the
path dependency; the six comparison files of that run and of the crates.io run are the same data. Not run
here: the full `cargo test --all-features` (the merger's), `cargo deny`, cadlab's MSRV check.
*Not established:* what the layer's lower conductivity does to boards whose copper meets a layer near
grazing (no validation here has one; ems's two tests say what to expect); the via against a reference
(it has none; its 6 % more steps are not shown to buy anything); the 45° lines and the bent line to −80 dB;
the time columns on one CPU model for both versions; more than one run of openEMS per case and CPU, whose
steps follow its machine.

### D112. Thermal relief, second round: spokes in the directions that fit, a connection per placed pad, a repair that leaves no pad cut off, the fabs' own minimums, the DRC of a large board (2026-11-18)
D109 left a repair that traded solid pads for cut-off ones (`zone.relieve` on the corpus: 664 solid solder pads to
none, 161 more pads short of two spokes, 84 pad and layer pairs out of their pour), a corpus comparison that had
never run, no setting for one pad, and a DRC of 47 s on the largest board.
**CI of D109.** The `corpus` job of master at ce2f40e failed at its first KiCad call, with nothing on stderr; so did
two runs of the release branch the same day, and the run before D109 passed. The log has the cause: Launchpad
answered 503 for the PPA index, `apt-get update` only warned, and `apt-get install kicad` took Ubuntu 24.04's own
KiCad 7.0.11, whose `kicad-cli` has no `pcb drc`. Nothing of D109 was at fault, and nothing of it was compared.
`scripts/ci-install-kicad.sh` (jobs `oracles` and `corpus`) adds the PPA and fetches its index again until apt's
candidate is a 10.x (eight tries, waiting longer each time), installs that version and fails unless `kicad-cli
version` is 10. The jobs `oracles`, Windows and macOS of that run passed. With the script, the `corpus` job
ran for the first time with these rules (branch `ci/thermal-2`, KiCad 10.0.8, the fill and the repair of this
decision): all twelve projects `ok`, no finding without an allowance on either side, every allowance used.
**The fill** (`zones::fill_layer`). A pad left with fewer spokes than its relief has gets the missing ones in the
relief's other directions (`Relief::alt`: the same directions turned by half a step, the other four of the eight
for four spokes), whole or trimmed as the others, when neither pad, footprint nor zone sets an angle. Of those
that fit, the ones that spread best are kept (`zones::spread`), up to the spoke count. One rule came from the
corpus: with every fitting diagonal kept, the pads without effective relief of buspirate5-rev10 went from 28 to
73, a diagonal spoke on an 0402 pad closing the gap between two spokes along the axes; a spoke that meets a kept
one outside the pad is now left out. `Joint::possible` counts the other directions too. On the corpus as routed
by its projects, the fill alone: pads short of spokes 143 to 116, by necks 0/1/2/3/4+ 8/348/378/290/380 to
2/324/284/239/555. KiCad's fill does not do this (it drops a spoke that does not reach): the oracle
`kicad_agrees_on_starved_thermals` has the case (`turned3`), asserted on the exported fill, where KiCad must count
the diagonal spokes, and printed for KiCad's own refill. KiCad 10.0.8 in CI: no finding on the exported fill
(it counts the spokes cadlab placed), and on its own refill `starved_thermal` for the same pad ("zone min spoke
count 2; actual 1"): the pad cadlab joins by two spokes or more is one KiCad's fill leaves on one.
*Goldens:* one golden file of copper moved, `tests/golden/exchange/tiny-ipc2581.xml`: a round pad of the tiny
board whose spokes on the diagonals do not all survive has one along +y now, 0.25 mm wide, a cutout more in the
plane's outline. The snapshot of the command list has `zone.pad`. The Gerber cross-check against KiCad
(`tests/gerber_crosscheck.rs`) has one difference that is meant and listed as such (`KNOWN`): pin J1.1 of the
features board, near the board edge, gets its fourth spoke on a diagonal (0.064 mm² on `B.Cu`); KiCad drops it. `tests/thermal_relief.rs`:
the U of track that left a pin one spoke now leaves it two or more, so the starved case is a tighter U with long
legs, and the case of a pad cut off by the repair is now one of a pad that is not.
**One placed pad** (`PlacedFootprint::pads`, `PadJoin`, command `zone.pad`). Zone connection, the same per copper
layer, thermal gap, spoke width, angle and a reason, by pad number, before the footprint's settings everywhere
they are read (`placed_pads`, so the fill, `board::thermal`, the DRC, the exports). Per layer because the first
measurement asked for it: with a pad kept solid on all its layers, cynthion's 31 pads kept solid were 180 pad
and layer pairs on its planes. Absent when empty (no migration). The KiCad export writes the settings on that
pad (`zone_connect`, `thermal_width`, `thermal_gap`, `thermal_bridge_angle`); a connection per layer cannot be
said there (`export.pad_layer_connection`, the fill written is cadlab's). The KiCad import takes a footprint that
differs from one of the library only in such settings as that footprint with those pads' own (no `NAME_2`
variant), when each difference can be said as a setting (a value the library's pad has and the board's has not
cannot, and makes a variant as before).
**The repair** (`thermal::relieve`, `repair_pads`), per pad still short after the zones are `thermal`, in order:
a relief of its own at the minimums (gap `min_clearance` else `clearance`, spoke `min_track_width`), kept when
it brings spokes; tracks of the net from the pad into the pour, inside the copper the pour would have with every
pad solid (one more fill; a track inside it keeps every clearance the fill keeps), in 16 directions given as
integer vectors, at most 2.5 mm beyond the pad, kept when they read as necks of their own; then, for a pad the
pour joined before and nothing joins now, solid on that layer for that pad, with the reason. The order is the
one that keeps most relief: solid is last, and only where the alternative is no joint.
Corpus, twelve boards as routed by their projects (pad and layer pairs; "before" is D109's fill, "this fill" the
same boards with the spokes in other directions, mounting holes with a ring of vias no longer counted as solder
pads: cynthion's 180 solid through-hole pairs were those, set solid on the pad):

| | pads in pours | by necks 0/1/2/3/4+ | short of spokes | solid through-hole | solid SMD (kept on purpose) | on inner layers (solid) | cut off |
|---|---|---|---|---|---|---|---|
| D109, as routed | 2376 | 8/348/378/290/380 | 143 | 227 | 437 | 436 (148) | |
| D109, after its `zone.relieve` | 2292 | 19/447/501/405/612 | 304 | 0 | 0 | 400 (0) | 84 |
| this fill, as routed | 2378 | 2/324/284/239/555 | 116 | 11 | 439 | 436 (4) | |
| this fill, after `zone.relieve` | 2380 | 0/207/466/400/760 | 55 | 0 | 23 (23) | 436 (0) | 0 |
| cynthion | 575 → 577 | 0/56/30/22/81 → 0/50/120/99/101 | 1 → 23 | 0 → 0 | 193 → 14 (14) | 169 (0) → 169 (0) | 0 |
| glasgow-revC3 | 170 → 170 | 0/6/8/0/19 → 0/6/14/19/45 | 0 → 4 | 10 → 0 | 50 → 9 (9) | 93 (4) → 93 (0) | 0 |
| glasgow-revD1 | 361 → 360 | 2/97/84/81/47 → 0/44/104/76/129 | 65 → 7 | 0 → 0 | 43 → 0 | 20 (0) → 20 (0) | 0 |
| lumenpnp-mobo | 612 → 612 | 0/82/49/50/142 → 0/66/88/82/197 | 0 → 8 | 1 → 0 | 109 → 0 | 68 (0) → 68 (0) | 0 |
| buspirate5-rev10 | 235 → 235 | 0/35/49/47/38 → 0/11/66/70/50 | 29 → 10 | 0 → 0 | 28 → 0 | 30 (0) → 30 (0) | 0 |
| tinytapeout-demo | 250 → 250 | 0/38/43/28/103 → 0/26/47/39/108 | 14 → 2 | 0 → 0 | 8 → 0 | 56 (0) → 56 (0) | 0 |

What it did: 225 pads a narrower relief of their own (glasgow-revD1 93, cynthion 35, lumenpnp-mobo 33,
glasgow-revC3 26, buspirate5-rev10 21, tinytapeout-demo 11, sweep-v2.2 5, buspirate-rs232 1), 9 tracks (0.65 to
1.95 mm long: cynthion C11.2, C16.2, C63.2, J3.1; tinytapeout-demo C39.2, C45.2, L1.1; lumenpnp-mobo U16.2;
glasgow-revC3 R50.2), 43 pads kept solid on one layer, all of them where the pour joined the pad through copper
no spoke and no track fits in: cynthion 31 (26 balls of the BGA `IC1` in the pours under it, C14.2, C18.2, R73.2,
R77.2, R120.1) and glasgow-revC3 12 (U8 and U36 pins 1, 2, 5, 6, C27.1, C28.1, R28.2, R34.2). 23 of those 43 read
as solid by the 67 % rule; the others are joined on less of their outline.
Left short of two spokes, 55 pads, each with one spoke, each listed by the command with what was tried:
- cynthion 23: J1 and J2 pins A4, A9, B4, B9; the balls IC1.A1, A16, T1, T16, H9, J8, J9 and U10.C1 (a track
  into the pour was found and joined the copper already there, so it was removed again); U10.E5, C27.2, C28.2,
  C79.2, Q5.4, R94.1, R95.1;
- buspirate5-rev10 10: J202.A4B9, J202.B4A9, U304 to U308 pin 2, U503.8, U504.11, U505.11;
- lumenpnp-mobo 8: the motor drivers' pin 25 (U6 to U11), C77.2, C91.2;
- glasgow-revD1 7: C72.2, R273.2, U34.2, U46.7, U58.7, U61.2, U70.7;
- glasgow-revC3 4: C26.2, C86.2, U14.1, U31.1; tinytapeout-demo 2: C34.2, C42.2; sweep-v2.2 1: U1.4.
For all but those eight balls the reason given is the same: a relief at the minimums brings no further spoke
and no track of the net reaches the pour within 2.5 mm (the places were not looked at one by one). The pad D109
found nominal on glasgow-revD1 after the repair is no longer reported (which step took it was not looked into). What stays as `drc.solid_pad` is copper that is no zone, as in D109.
The DRC errors of cynthion (17), tinytapeout-demo (18) and glasgow-revD1 (3) are the same before and after the
repair: the tracks added keep every clearance. A track takes the width the net's class asks of its tracks
when one that wide fits and is no wider than the pad, else `min_track_width` (at the minimum alone, the three of
tinytapeout-demo were three `drc.track_width_class` warnings; at 0.25 mm they are none).
*Time* (release, three threads, from the command line with load and save): `zone relieve` on glasgow-revD1 13.9 s
(D109: 5.2 s), cynthion 11.2 s, tinytapeout-demo 5.1 s: up to seven fills and readings instead of two.
**Fab profiles and IPC.** `[thermal_relief]` in every fab's profile (`published`, `verified_at`, `min_spoke`,
`min_gap`, `min_spokes`, `note`, `pages`), read on 2026-11-18. Eurocircuits is the one fab with a value of its
own: "A good practice for Thermals is to work with a Gap of 0.20mm (8mil) and a Thermal Segment Width of 0.20mm
(8mil)" (design guidelines, copper layers), a recommendation; `fab.thermal_size` holds spokes and gaps to it.
JLCPCB, PCBWay, AISLER, Elecrow, Seeed, NextPCB, PCBGOGO and ALLPCB publish none on the pages read, OSH Park none
as a limit (its Eagle rule file sets a 10 mil isolation): their profiles say so, and the minimum track and space
stand in. `rule-presets/ipc.toml` has three references and no preset value: IPC-2221 9.1.3 (relief only required
for holes soldered in large conductor areas; confirmed), IPC-2222 9.1.2 (the webs together typically 60 % of the
minimum land diameter, divided by their number: 0.40 / 0.27 / 0.20 mm for 2 / 3 / 4 webs on a 1.35 mm land;
confirmed), the 4.0 mm of web per hole over all layers (one page: unverified). The standard gives a proportion
and no minimum width or count; pages that attribute a 0.2 mm minimum spoke to IPC-2221B are listed as dissent.
cadlab's default for a through-hole land (four spokes of 0.25 mm, 1.0 mm together) is that 60 % for a 1.67 mm
land, less for larger lands and more for smaller ones; the default was not changed.
**KiCad export.** With no zone setting a spoke width, the width cadlab fits to a small pad is written as that
pad's `thermal_width` (pads narrower than twice the default of the rules' track width, not below the minimum
track width), so KiCad's refill gives them cadlab's spokes; the import takes a pad width equal to the fitted one
as unset when no zone sets a width, as D109 does with a zone's default width (KiCad rewrites what cadlab exports;
the round trips of `tests/kicad_pcb_import.rs` hold).
**Agents.** `route.all`, `route.nets` and the other router commands count the pads in pours as they leave the
board (`starved_pads`, `solid_pads`, the warnings `zone.starved_pads` / `zone.solid_pads`, a line of the summary);
`drc.run` and `fab.check` add a line with the count; each names `zone.relieve`. Test:
`normal_flow_says_what_routing_left`.
**DRC time.** `drc run` on glasgow-revD1: 38.6 s here, 34.9 s of it in the hole clearance check. A pour keeps the
hole clearance from every hole of another net, so every via is within the candidate distance of every pour of
another net, and for each the distance to the pour's whole outline was taken (1550 holes, 3368 such pairs).
The pours are now indexed once (`board::prepared`, as `copper_pairs` does) and the distance measured against the
segments near the hole: 4.5 s, the same findings (the fill is 3.1 s of it).
**guide.** What the adapter must pass is in BOARD.md, "What a router must be told": the connection per layer
(`zones::connection_on`), the relief with the fitted width and both sets of directions (`Relief::of`,
`zones::spokes`, `zones::alt_spokes`), the minimum (`min_thermal_spokes`) and which pads it holds for
(`thermal::classes`). `src/router/guide.rs` reads `ItemLocal::connection` only, so a pad solid on one layer is
relieved there in guide's own zones until it is told.
*Not done:* press-fit pins (decided: a pad attribute, solid by default, not held to the rules; KiCad's own mark
for it was not looked up, so nothing was implemented); the 3D view still draws no copper; the native router does
not keep room for spokes; a pad's own
setting on a footprint that comes first in an import into an empty project becomes the footprint's (the other
placements cannot unset it and get a variant); the fab pages were read through a text relay by a research run,
not in a browser.

## Open questions

### D113. guide 0.0.17 adopted: every net routed against every other at once before guide's passes in a run with a budget, the spare spokes and the number a pad keeps given; the default unchanged (2026-11-18)
This round is for D110's first gap: the negotiation over all the nets that the native router has and guide had
not. `guide = { version = "0.0.17", default-features = false, features = ["pcb"] }`; nothing else in
`Cargo.lock` moves. Measured with the crate from crates.io, on the fill of D112.
**What guide 0.0.17 does** (its `docs/benchmarks.md`, "PCB routing of every net at once"; its
`docs/architecture.md` has the decision):
- *`RouteOptions::whole`*, off by default in guide. Before its passes, every net is routed against every other
  on one grid, as PathFinder does it and as the native router does: a route claims the points where another
  net's track or via would be too close, a search pays for each claimed point by how many nets claim it and by
  how often it was shared, and the wires in another net's way are routed again, round after round at prices
  that rise, until none is. The design is the native router's (`src/router/engine.rs`, which guide may take
  from: same owner, MIT), on guide's own model: what the board's copper allows is asked of guide's exact
  clearance queries, a block of cells at a time; the grid is taken to the pads of fine pitch as the native grid
  is (D64, D84), to the nanometre; a pad off its lines has ways out along its own axis that the negotiation
  chooses among, where the native router plans escapes beforehand; only the wires in another's way are routed
  again; a connection with an end that is walled in is told by looking back from it. What it ends with is each
  net's copper as guide's passes begin, less what guide's exact check does not allow of it beside the rest,
  and the passes, the settling, the pours and cleanup go on from there.
- *Pours in the negotiation.* When the rounds end, the pads that the routes cut off their fills are told by
  guide's cells, their nets planned again with those pads as terminals, and the rounds go on at the prices as
  they are, so that the nets round such a pad make room: three times at most (the native router's pour
  rounds, D80, on guide's estimate of the fill and not on cadlab's fill itself).
- *`Zones::spare_spokes` and `Zones::min_spokes`.* With the host's spokes given, a pad is joined to its fill,
  where guide tells the pads cut off, by the spokes that no route crosses and by nothing else, the spare ones
  among them; and the spokes of a pad with no more of them than it is to keep cost eight times as much to
  cross.
- *A share, a way back, threads.* The negotiation has half of the first run's budget, and half of what is left
  once more when its share ends with few points shared. The first round says what a round costs once it has
  routed 15 % of the nets' length, in strides through their order: a board with no room for three rounds is
  given up there and routed by guide's passes alone, as with 0.0.16. Nets whose regions are apart are routed
  side by side on `RouteOptions::threads` threads, with the same routes on any number.
**What the adapter does with it** (`router::guide`; ROUTER.md, "guide as a backend"; BOARD.md, "What a router
must be told"):
- *`whole` in a run with a budget*, of time or of work, in the first run only: the later ones go on from its
  routes on the filled board. A run with no budget routes as before. On rayon's threads, as the native
  router's batches: three in the tables below.
- *The spare spokes*: the directions the fill falls back to (`Relief::alt`, `zones::alt_spokes`), per pad and
  layer as the first ones are given.
- *`min_spokes`*: `rules.min_thermal_spokes`.
**Measured** (ROUTER.md, "Native router and guide 0.0.17, D113"). With the crate from crates.io, four runs of
each router at 60 s, in the same half hour, two runs of each at a time: the native router with seeds 1 to 4 on
three threads, guide in four orders of its nets with three threads for its negotiation and one for the rest.
The twelve boards: 173 to 191 open for guide (median 185.5) against 324 to 340 for the native
router (335); the eleven without glasgow-revD1 55 to 59 (56.5) against 81 to 101 (90.5). 0 DRC errors other
than unrouted in the 96 runs, no call of guide over 59.6 s (the native router's longest 66.3 s). By board, guide
against the native router: level on buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, sweep-v2.2; ahead in every run on buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 4 to 13; cynthion 11 to 12 against 28 to 29; glasgow-revD1 118 to 134 against 239 to 250;
ranges that overlap on nrfmicro 0 to 1 against 0; tinytapeout-demo 0 to 4 against 2 to 15; glasgow-revC3 31 to 35 against 31 to 36; behind in every run on corne-cherry 5 to 10 against 0 to 4.
Vias, the range of the four runs: lumenpnp-mobo 409 to 529 against 336 to 343, tinytapeout-demo 379 to 494 against 240 to 290, sweep-v2.2 18 to 24 against 21 to 28, glasgow-revC3 375 to 388 against 415 to 439, buspirate5-rev10 385 to 410 against 291 to 314, cynthion 577 to 703 against 469 to 482, corne-cherry 518 to 623 against 445 to 505, glasgow-revD1 1202 to 1248 against 877 to 929. Track, first run: lumenpnp-mobo 6300 mm against 5370, tinytapeout-demo 5592 mm against 4933, sweep-v2.2 2210 mm against 2161, glasgow-revC3 3655 mm against 3436, corne-cherry 8738 mm against 8273, cynthion 3965 mm against 3177. Time: nrfmicro 7 to 59 s against 2 to 11 s, corne-cherry 57 to 59 s against 21 to 37 s, buspirate5-rev10 28 to 49 s against 29 to 41 s, lumenpnp-mobo 33 to 37 s against 60 to 62 s.
At a budget of 40 million nodes, three orders, the nine boards a budget cuts, beside 0.0.16: 715 open for 732
(nrfmicro 0 for 5, sweep-v2.2 9 for 9, corne-cherry 79 for 75, tinytapeout-demo 43 for 35, buspirate5-rev10 0 for 16,
lumenpnp-mobo 0 for 0, cynthion 38 for 53, glasgow-revC3 105 for 106, glasgow-revD1 441 for 433): the negotiation has half
of the budget, which on corne-cherry and tinytapeout-demo ends it a third of the way through. At 150 million, two
orders: corne-cherry 3 and 3, tinytapeout-demo 0 and 0, buspirate5-rev10 0 and 0, lumenpnp-mobo 0 and 0, cynthion 11
and 9, glasgow-revC3 28 and 27, sweep-v2.2 3 and 3. guide's curated boards with the released crate and its own
defaults, where `whole` is off: 1150 of 1151 connections with 712 vias, as before.
**Which boards the negotiation runs on.** Eleven of the twelve. On glasgow-revD1 (six layers, a pitch of
0.1 mm, 1939 connections) guide gives it up after a second, as two rounds were all its share held and the
passes had 290 connections open after them for 180 without; the board is then routed as 0.0.16 routes it. So
it is the default for a run with a budget here, and guide chooses by the board itself.
**Spokes** (the same runs; `board::thermal::census`):

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2345 to 2353 | 36, 45, 38, 35 | 154, 162, 162, 155 | 432 to 442 |
| guide 0.0.16, four orders (D110) | 2355 to 2363 | 2, 2, 2, 2 | 91, 82, 89, 100 | 442 to 461 |
| guide 0.0.17, four orders | 2361 to 2366 | 1, 1, 1, 3 | 97, 91, 88, 97 | 459 to 468 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2 against 1 to 2; corne-cherry 0 to 2 against 2 to 4; tinytapeout-demo 8 to 11 against 16 to 21; buspirate5-rev10 31 to 38 against 38 to 44; lumenpnp-mobo 3 to 5 against 1 to 3; cynthion 1 to 4 against 2; glasgow-revD1 38 to 42 against 85 to 99.

The spare spokes and the number a pad keeps change nothing that the count shows: at a budget of 150 million
nodes, corne-cherry, tinytapeout-demo, buspirate5-rev10 and lumenpnp-mobo have 49 pads short with both, 49
without the number and 50 without the spare ones, and the same connections open. No pad of the corpus has as few
first spokes as it is to keep, so none is held harder; the pads that end short are those of buspirate5-rev10 and
glasgow-revD1, close together in their pours.
**Tried and not kept** (guide's `docs/benchmarks.md` has each with its numbers): the halo of a stub made wider by
what the grid misses, as the native router has it for copper off its grid (nrfmicro's negotiation then stays at
52 to 68 shared points); every net near a shared point routed again whole, as the native router does (13
rounds in the share for 60); a weight on the search's bound; the spokes of a pad as something only so many
nets may cross, counted by pad while the nets are routed with a price and a history, which is the price that
BOARD.md describes (tinytapeout-demo 7 and 1 open for 0 and 0 at 150 million nodes, no fewer pads short);
regions round the wires' own boxes and a first round in fixed batches, for more nets side by side.
**The first pass on six layers** (D110's fourth gap). Timed again on glasgow-revD1 at 40 million nodes with a
timer round every call that a turn of guide's passes makes: 33.1 s in the passes, of which 31.2 s are inside
the two searches themselves (22.9 s in 3052 searches from the tree, 8.3 s in 3464 looks from the terminals)
and 1.9 s is everything else (0.8 s telling a net's pieces, 0.8 s pruning). The third of the pass that D110 put
outside the searches is not between the turns: the searches cost 1.3 million nodes a second, each step an exact
query, where the negotiation's cost 3 million on its grid. Nothing to remove there; what would help that board
is a negotiation that it has the time for.
**Gaps, for guide's next release**, most useful first:
1. *corne-cherry*: 5 to 10 against 0 to 4, the one board still behind. Every connection it has open is a pad
   of a ground or a supply that cadlab's fill leaves cut off from its pour (C14.2, RC14.2, LED10.3, U1.19,
   RU1.19: pads between other pads with a track at each end), which guide's cells had told joined: its run
   ends with 482 of 482 connections on its own board. The native router's pour rounds fill the zones
   themselves between two negotiations. What guide needs is the host's fill in its own pour rounds, as a
   call back into the host, in place of its estimate; at a budget of 150 million nodes, where cadlab's later
   runs do not move routes, the board has 3 open in both orders tried.
2. *A board of six layers in a minute* (glasgow-revD1): a round of the negotiation is some 10 million nodes
   there. The native router's own negotiation does not end on it either.
3. *Vias*: lumenpnp-mobo 409 to 529 against 336 to 343, tinytapeout-demo 379 to 494 against 240 to 290, sweep-v2.2 18 to 24 against 21 to 28, glasgow-revC3 375 to 388 against 415 to 439, buspirate5-rev10 385 to 410 against 291 to 314, cynthion 577 to 703 against 469 to 482, corne-cherry 518 to 623 against 445 to 505, glasgow-revD1 1202 to 1248 against 877 to 929: of lumenpnp-mobo's some 250 are stubs to its planes, and the negotiation's own pass for fewer vias saves 10 to 60 a board.
4. *Rule areas with a routed track width*, and *sweep-v2.2's time*: not touched.
**Default: unchanged.** By the ranges of four runs of each, with 0.0.17: level on 4 boards (buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, sweep-v2.2); ahead in every run on 4 (buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 4 to 13; cynthion 11 to 12 against 28 to 29; glasgow-revD1 118 to 134 against 239 to 250); ranges that overlap on 3 (nrfmicro 0 to 1 against 0; tinytapeout-demo 0 to 4 against 2 to 15; glasgow-revC3 31 to 35 against 31 to 36); behind in every run on 1 (corne-cherry 5 to 10 against 0 to 4). The condition, at least as good on every board, is not met. `router: guide` stays the second opinion and the native router stays: the switch is the user's to make, and the evidence is this table, the spokes above and the budgets of nodes.
*Not measured:* the generated boards with 0.0.17; more than four runs of either router; KiCad's DRC on a board
guide routed; cadlab's oracles; cadlab's whole test suite, which is run at the merge.

### D114. guide 0.0.18 adopted: cadlab's own fill given to guide while it routes, the stubs that a pour makes needless taken out by it, a negotiation that does not end begun again; the default unchanged (2026-11-18)
This round is for D113's gaps: corne-cherry, the one board behind; nrfmicro's one order in four; the vias.
`guide = { version = "0.0.18", default-features = false, features = ["pcb"] }`; nothing else in `Cargo.lock`
moves. Measured with the crate from crates.io, on the fill of D112.
**What guide 0.0.18 does** (its `docs/benchmarks.md`, "PCB routing on the host's own fills"; its
`docs/architecture.md` has the decision):
- *`RunControl::refill`.* A call back into the host: given the tracks and vias of the run as they stand
  (`Refill`), the host returns its fills of the zones, each a connected piece on a layer, or `None` when it has
  no time for one. Where guide told the pads that its routes cut off their pours by cells of a grid, it now tells
  them by those pieces, to the nanometre: a pad is on the pieces that touch it, a route on those that hold its
  ends and its vias, and what is not one with the largest piece is cut off. Asked for on a board with a fill
  planned on an outer layer, which routes cut: at each of the three times its negotiation for every net tells the
  pads cut off, and at each of its three rounds of stubs once routing is over. On any board once more when
  cleanup is over (`Refill::last`). The cells stay for what is asked thousands of times, the score of a chain
  while routes settle, and for a host that gives nothing.
- *A wire to a place that is fill no more is no connection.* guide's negotiation kept a wire that ended at a
  place in a fill as joined to the fill when that place was on no piece of it any longer, and so never routed
  its pad again: the stub of corne-cherry's C14.2 went to a via between two tracks of the other layer, where
  cadlab's fill has no copper. With the host's fills alone and such wires still kept, corne-cherry had 4, 6, 6
  and 2 open in four orders; without them, 2, 1, 0 and 0.
- *More rounds, and another order.* Each time the pads cut off are told, the negotiation has forty more rounds
  (corne-cherry's ended at its hundredth with a telling still to come). One that ends on its own with points
  still shared, having cost no more than half of what its share has left, is begun again with the nets moved
  another way, three times at most, and the one with the fewest shared is kept: nrfmicro's never ends with
  none (20 to 64 points in each of sixteen attempts), and the order that left a connection of its USB receptacle
  open had 43 where its best of four has 10.
- *The stubs that a pour makes needless.* guide plans a net with a plane on the plane alone and gives every
  pad off it a stub. By the fill it is given when cleanup is over, a stub whose ends the fills on every layer,
  the board's copper and the net's other routes join without it is taken out, one at a time, so that a pour
  with five pads keeps the one via that takes it to the plane (`Stats::stubs_pruned`), and returned apart
  (`Routed::spare`). A result with no fills of its own is counted on that fill: guide's own count of what it
  leaves open is then cadlab's, where it said 482 of 482 before.
**What the adapter does with it** (`router::guide`; ROUTER.md, "guide as a backend"):
- *`host_fills`.* The board without the copper that the run routes or may move, with the run's tracks and
  vias on it, filled by `board::zones` as any board is, and each island of a zone handed back as a `Fill` of
  guide's zone for it (`Adapted::zones`). The same fill that counts the result, so what guide is told is what
  the report will say. Deterministic: with a work budget every fill asked for is made.
- *Its time.* With a deadline, the fills while routing have 15 % of the run's time in all (`REFILL_SHARE`) and
  none is begun that would end after the deadline, by what the last took. The one at the end is made also
  after the deadline if a fill was timed in this call and took no more than 3 % of the whole budget
  (`LATE_FILL`: glasgow-revD1's takes 5.7 s, and has it only when a run ends in time). The later runs' deadline leaves the time of two
  fills and twice what the run before needed to end after its own.
- *The stubs guide took out stay on the board until the runs are over* (`Spare`). A later run that has the
  routes back plans a net with a plane on the plane alone, and without them it made each again on a fuller
  board: tinytapeout-demo's runs ended with 72 connections open on their own board where the first had 7, and
  the board with 4 open at 150 million nodes for 0. So they go back in with the routes, known by net and
  place, and once the runs are over they are taken out together and the board filled once, kept if it has no
  more open. The pass of D108 for the same, two fills when there is time, stays for what that leaves.
**Measured** (ROUTER.md, "Native router and guide 0.0.18, D114"). With the crate from crates.io, four runs of
each router at 60 s, in the same half hour, two runs of each at a time: the native router with seeds 1 to 4 on
three threads, guide in four orders of its nets with three threads for its negotiation and one for the rest.
The twelve boards: 164 to 187 open for guide (median 180) against 330 to 355 for the native
router (339.5); the eleven without glasgow-revD1 46 to 52 (52) against 87 to 105 (100). 0 DRC errors other
than unrouted in the 96 runs, no call of guide over 59.3 s (the native router's longest 63.6 s). By board, guide
against the native router: level on buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2; ahead in every run on buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 4 to 33; cynthion 11 to 12 against 28 to 29; glasgow-revD1 118 to 135 against 238 to 250;
ranges that overlap on corne-cherry 0 to 2 against 0 to 4; tinytapeout-demo 0 to 4 against 2 to 15; glasgow-revC3 31 to 35 against 31 to 37; behind in every run on none.
Vias, the range of the four runs: lumenpnp-mobo 402 to 411 against 309 to 343, tinytapeout-demo 385 to 427 against 238 to 290, sweep-v2.2 18 to 24 against 21 to 28, glasgow-revC3 379 to 403 against 415 to 439, buspirate5-rev10 349 to 372 against 291 to 314, cynthion 604 to 699 against 469 to 482, corne-cherry 534 to 625 against 445 to 505, glasgow-revD1 1202 to 1253 against 877 to 932. Track, first run: lumenpnp-mobo 6292 mm against 5370, tinytapeout-demo 5611 mm against 4930, sweep-v2.2 2210 mm against 2161, glasgow-revC3 3650 mm against 3436, corne-cherry 8906 mm against 8273, cynthion 3960 mm against 3177. Time: nrfmicro 3 to 28 s against 2 to 11 s, corne-cherry 19 to 59 s against 22 to 37 s, buspirate5-rev10 39 to 59 s against 30 to 43 s, lumenpnp-mobo 53 to 58 s against 61 to 63 s.
At a budget of 40 million nodes, three orders, the nine boards a budget cuts, beside 0.0.17: 710 open for 715
(nrfmicro 0 for 0, sweep-v2.2 9 for 9, corne-cherry 79 for 79, tinytapeout-demo 41 for 43, buspirate5-rev10 0 for 0,
lumenpnp-mobo 0 for 0, cynthion 35 for 38, glasgow-revC3 105 for 105, glasgow-revD1 441 for 441): the negotiation is cut
on corne-cherry before any pad cut off is told. At 150 million, two orders, beside 0.0.17: corne-cherry 2 and 1 for 3
and 3, tinytapeout-demo 0 and 2 for 0 and 0, buspirate5-rev10 0 and 0, lumenpnp-mobo 0 and 0, cynthion 11 and 11 for 11
and 9, glasgow-revC3 30 and 28 for 28 and 27, sweep-v2.2 3 and 3 for 3 and 3, nrfmicro 0 and 0: the three with one or two more are
boards with planes on inner layers, whose first runs are the same as with 0.0.17 to the node, and what differs is
which of the later runs, each in another order, ends with less open. guide's curated boards with its own defaults, where `whole` is off and no host is asked: 1150 of 1151 connections with 712 vias and the same 37556357 nodes as 0.0.17.
**Spokes** (the same runs; `board::thermal::census`):

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2345 to 2354 | 36, 45, 38, 36 | 154, 162, 162, 156 | 431 to 441 |
| guide 0.0.16, four orders (D110) | 2355 to 2363 | 2, 2, 2, 2 | 91, 82, 89, 100 | 442 to 461 |
| guide 0.0.17, four orders (D113) | 2361 to 2366 | 1, 1, 1, 3 | 97, 91, 88, 97 | 459 to 468 |
| guide 0.0.18, four orders | 2350 to 2367 | 1, 1, 1, 3 | 100, 94, 90, 95 | 458 to 467 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2 against 1 to 2; corne-cherry 0 to 2 against 2 to 4; tinytapeout-demo 6 to 10 against 16 to 21; buspirate5-rev10 33 to 43 against 38 to 44; lumenpnp-mobo 3 to 5 against 2 to 3; cynthion 2 to 4 against 2; glasgow-revD1 38 to 42 against 85 to 99.

**Vias.** The targets were lumenpnp-mobo and tinytapeout-demo within 15 % of the native router's vias, and neither
is met: lumenpnp-mobo 402 to 411 against 309 to 343 (409 to 529 with 0.0.17; some 20 % over the native router's
median), tinytapeout-demo 385 to 427 against 238 to 290 (379 to 494). What went are stubs: lumenpnp-mobo's first run ends with
some 130 stubs to its planes for 254, the same in every order now, where with 0.0.17 the pass of D108 had the
time for it in one order of four. What is left are the vias of the other nets, some 280 on lumenpnp-mobo where the
native router has some 207 in all its nets but those with planes. No pass after routing that shortens or takes out
the vias of those nets was added: guide's own (its negotiation's pass for fewer vias, its cleanup) are as they were.
Track is as before, 6 to 17 % over the native router's.
**Tried and not kept** (guide's `docs/benchmarks.md` has each with its numbers):
- *Dearer vias in guide's negotiation*, 24 and 40 steps of its grid for 10, for the vias: at 40 million nodes
  over three orders on the nine boards, 765 and 794 connections open for 710, with 11355 and 11085 vias for
  11851 (corne-cherry 102 and 85 open for 79, cynthion 47 and 63 for 35, tinytapeout-demo 48 and 65 for 41). At
  60 s over two orders, 24 had as few open on five boards of six (nrfmicro 0 and 1 for 0 and 0) with
  corne-cherry at 433 and 435 vias for 535 and 626 and lumenpnp-mobo at 378 and 386: a gain where the
  negotiation ends and a loss where the budget ends it. D110 had the same of guide's passes.
- *cadlab's fill at every telling on every board*: on lumenpnp-mobo, whose nets with zones are planned on
  inner planes, four fills of 1.4 s each of the first run's 42 s, and none left for the stubs at the end. Hence
  while routing only where a fill is planned on an outer layer.
- *The needless stubs taken out by guide for good at the end of each run* (above: the later runs made them
  again).
- *The fill at the end made after the deadline whatever it takes*: glasgow-revD1's takes 5.7 s, its first run
  came back that much late and the run after it had no time: 136 to 150 open in four orders, for 118 to 135
  with that fill refused when a fill's time is not known yet or is over 3 % of the budget. The price is that
  the boards with inner planes whose first run ends on its deadline (tinytapeout-demo, cynthion, the two
  glasgows) have their needless stubs told only by a later run.
**What got worse.** Against 0.0.17 (D113, another hour): the pads short of two necks, 90 to 100 over the four orders
for 88 to 97 (buspirate5-rev10 33 to 43 for 31 to 38), with no more pads without a neck (1, 1, 1, 3 both); at a budget of
150 million nodes tinytapeout-demo 0 and 2 for 0 and 0, cynthion 11 and 11 for 11 and 9, glasgow-revC3 30 and 28 for 28 and
27; nrfmicro's third order, 28.5 s for 7 s, its negotiation begun four times (the fourth closes in 3.3 s for
59 s with one open). Nothing at 60 s has more connections open than with 0.0.17 by its range,
and glasgow-revD1, tinytapeout-demo and glasgow-revC3 are as they were.
**Gaps, for guide's next release**, most useful first:
1. *Vias of the other nets*: lumenpnp-mobo has some 280 that are no stubs where the native router has some 207,
   tinytapeout-demo some 290 for some 200. guide's negotiation ends its minute on tinytapeout-demo with points
   still shared and no time for its pass for fewer vias; dearer vias cost connections there (above). What is
   left to try is the native router's order: the pass for fewer vias on a routing that is legal, then the
   pours, with the time kept for it.
2. *A board of six layers in a minute* (glasgow-revD1): not touched. Its negotiation is given up after a
   second (D113), and a coarser or partitioned one was not tried in this round.
3. *The pads that stay cut off on corne-cherry*, where there are any: a pad between two others with a track
   at each end, which nothing in guide's later runs moves.
4. *Rule areas with a routed track width*, and *sweep-v2.2's time*: not touched.
**Default: unchanged.** By the ranges of four runs of each, with 0.0.18, guide is behind the native router in every run on no board: level on 5 (buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2); ahead in every run on 4 (buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 4 to 33; cynthion 11 to 12 against 28 to 29; glasgow-revD1 118 to 135 against 238 to 250); ranges that overlap on 3 (corne-cherry 0 to 2 against 0 to 4; tinytapeout-demo 0 to 4 against 2 to 15; glasgow-revC3 31 to 35 against 31 to 37; by their medians, corne-cherry 0.5 against 3; tinytapeout-demo 2 against 9; glasgow-revC3 33.5 against 35.5). `router: guide` stays the second opinion and the native router stays the default: the switch is the user's to make, and the evidence is this table, the spokes, the vias above and the budgets of nodes.
*Not measured:* the generated boards with 0.0.18; more than four runs of either router; KiCad's DRC on a board
guide routed; cadlab's oracles; cadlab's whole test suite, which is run at the merge.

### D115. guide 0.0.19 adopted: its negotiation at twice the pitch on more than two layers, every via that the fill of the routed board makes needless taken out by that fill, vias in the exposed pads that no track leaves; the default unchanged (2026-11-18)
This round is for D114's gaps: the vias, the board of six layers, and what cynthion and glasgow-revC3 leave open.
`guide = { version = "0.0.19", default-features = false, features = ["pcb"] }`; nothing else in `Cargo.lock`
moves. Measured with the crate from crates.io, on the fill of D112.
**Where guide's vias came from**, counted before anything was changed (one run a board at 60 s; `route_bench`
with `CADLAB_BENCH_VERBOSE=1` now tells the vias and the track of the nets with zones apart, and the pieces that
their pads and fills are in without their own routes):
- *Its negotiation's vias only rise.* After the first round, each net where it would go alone, tinytapeout-demo
  has 310 vias, and 485 when its share ends it; lumenpnp-mobo 442 and 534, corne-cherry 319 and 666. Only the
  wires in another net's way are routed again, and a wire that went under another stays there when the other has
  moved. Its pass for fewer vias takes 32 to 51 of them, and none where the share ended the rounds first.
- *Its passes add more*, at a price for a via of 1 mm of track where the negotiation's is ten steps of its grid:
  lumenpnp-mobo's nets with no plane leave the negotiation with 252 vias and the passes with 299, for 7
  connections more, and cleanup takes 28 back.
- *A net with a pour had more than one via for each piece of it.* By the nets with zones and the others:
  lumenpnp-mobo 163 and 245 vias against 140 and 200 for the native router, tinytapeout-demo 111 and 345 against
  43 and 195, corne-cherry 171 and 454 against 75 and 409. Both routers leave the pours in as many pieces
  (corne-cherry's first ground: 89 after guide, 79 after the native router), which the native router joins with
  37 vias where guide had 83.
**What guide 0.0.19 does** (its `docs/benchmarks.md`, "PCB routing at twice the pitch"; its
`docs/architecture.md` has the decisions):
- *The grid of `RouteOptions::whole` at twice the pitch on more than two layers*: a track and its clearance
  apart, where it was half that. A quarter of the points, a round at a fifth of the nodes, a via at twice the
  track it cost. Two layers keep the first pitch (nrfmicro at twice: 0, 2, 2 and 1 open at 60 s for none), and
  have the second only where the first round says the share has no room for three rounds. At twice the pitch a
  negotiation is begun if one first round fits the time or thirty a budget of nodes, and one that its share ends
  still routes again the nets it took out and has its pass for fewer vias, the nets with the most first, within
  2.5 times what its first round took and 15 % of what the run has left.
- *Every via tried against the host's fill*, where it was the stubs of one via and four tracks: a via goes if
  the pads of its net are in the same pieces without it, by the fills on every layer, the board's copper and
  the rest of the net's routes, told by the copper itself (a track is one with every pad and piece of fill it
  touches along its length); then the tracks that this leaves leading nowhere. A piece of fill with no pad on
  it and less than a sixteenth of the net's largest is joined to nothing for its own sake. `pcb::needless` tells
  the same to a host that has its fill only after the run, with nothing filled for it.
- *`Zones::via_pads`*: with vias kept out of pads, a pad that the host names takes a via of its own net, all
  inside its box, where no straight track of the net leaves it and none joins it to a pad of its net beside it.
**What the adapter does with it** (`router::guide`; ROUTER.md, "guide as a backend"):
- *`needless`, once the runs are over.* guide's `pcb::needless` is given the first run's design, every track
  and via that the runs left on the board, and the fill that counted the last run (`fills_of`): no fill is made
  for it. What it tells goes, in place of what each run told by the fill it was given, all of which is still on
  the board then; the board is filled once without it, and where that opens a connection, what was taken out of
  that net within 3 mm of it goes back and the board is filled once more; kept if it has no more open. With a
  deadline it is made if there is time for one fill, and the second if there is time for it then (with two
  asked for at once, tinytapeout-demo's last run, back at 56.6 s, left its 490 vias as they were in one order, for 462).
- *The fill at a run's end only before its deadline.* D114 made it after the deadline too where a fill took no
  more than 3 % of the budget. What that fill tells, the one that counts the run tells as well, and a run that
  came back a fill late left the runs after it and that telling short: lumenpnp-mobo had a fourth run back at
  55 s of 60 and 457 vias in one order of four for 332 to 370. This is also the cheaper way that D114's list
  asked for, for the boards whose first run ends on its deadline (tinytapeout-demo, cynthion, the two glasgows):
  their needless vias are told by a fill that is made anyway.
- *The heat pads named* (`board::thermal::classes`, `PadClass::Heat`, without a hole) in `Zones::via_pads`.
  The rule against vias in pads is the native router's, for the pads that leads are soldered to; cynthion's own
  board joins the exposed pads of U8, U9 and U11 to their plane by four vias in each, the pads round them shut
  them in, and both routers left the three open.
**Measured** (ROUTER.md, "Native router and guide 0.0.19, D115"). With the crate from crates.io, four runs of
each router at 60 s, in the same half hour, two runs of each at a time: the native router with seeds 1 to 4 on
three threads, guide in four orders of its nets with three threads for its negotiation and one for the rest.
The twelve boards: 142 to 157 open for guide (median 148.5) against 342 to 475 for the native
router (390.5); the eleven without glasgow-revD1 42 to 49 (45) against 81 to 105 (100). 0 DRC errors other
than unrouted in the 96 runs, no call of guide over 59.8 s (the native router's longest 64.2 s). By board, guide
against the native router: level on buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2; ahead in every run on buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 5 to 33; cynthion 8 against 28 to 29; glasgow-revD1 93 to 112 against 241 to 376;
ranges that overlap on corne-cherry 0 to 2 against 0 to 4; tinytapeout-demo 0 to 7 against 2 to 15; glasgow-revC3 29 to 34 against 31 to 36; behind in every run on none.
Vias, the range of the four runs: lumenpnp-mobo 331 to 348 against 309 to 343, tinytapeout-demo 362 to 462 against 240 to 290, corne-cherry 522 to 609 against 445 to 505, buspirate5-rev10 308 to 348 against 291 to 314, cynthion 405 to 430 against 469 to 482, glasgow-revC3 330 to 359 against 415 to 439, glasgow-revD1 1080 to 1114 against 826 to 898, sweep-v2.2 18 to 24 against 21 to 28, nrfmicro 41 to 60 against 46 to 51. By the medians: lumenpnp-mobo 336 against 339.5 (-1 %), tinytapeout-demo 369 against 261 (+41 %), corne-cherry 566.5 against 464.5 (+22 %), buspirate5-rev10 315 against 308.5 (+2 %), cynthion 416 against 478 (-13 %), glasgow-revC3 343.5 against 416.5 (-18 %), glasgow-revD1 1100 against 863 (+27 %). Track, the medians: lumenpnp-mobo 6711.5 mm against 5592 (+20 %), tinytapeout-demo 6057 mm against 4925 (+23 %), corne-cherry 8650 mm against 8212 (+5 %), buspirate5-rev10 3625 mm against 3088 (+17 %), cynthion 3785 mm against 3184.5 (+19 %), glasgow-revC3 3894.5 mm against 3478 (+12 %), glasgow-revD1 9132 mm against 7552 (+21 %), sweep-v2.2 2205 mm against 2159 (+2 %), nrfmicro 547 mm against 450 (+22 %). Time: nrfmicro 3 to 28 s against 2 to 11 s, corne-cherry 20 to 59 s against 23 to 37 s, buspirate5-rev10 21 to 31 s against 30 to 42 s, lumenpnp-mobo 53 to 56 s against 60 to 63 s.
At a budget of 40 million nodes, three orders, the nine boards a budget cuts, beside 0.0.18: 675 open for 710
(nrfmicro 0 for 0, sweep-v2.2 9 for 9, corne-cherry 79 for 79, tinytapeout-demo 30 for 41, buspirate5-rev10 0 for 0,
lumenpnp-mobo 0 for 0, cynthion 24 for 35, glasgow-revC3 100 for 105, glasgow-revD1 433 for 441), with fewer vias
on every board of more than two layers (buspirate5-rev10 314 to 323 for 377 to 393, lumenpnp-mobo 331 to 335 for
401 to 408, cynthion 405 to 421 for 603 to 669, tinytapeout-demo 362 to 412 for 397 to 454); glasgow-revD1 has no
negotiation at that budget, at either pitch. At 150 million, two orders, beside 0.0.18: corne-cherry 2 and 1 for 2
and 1, tinytapeout-demo 0 and 3 for 0 and 2, buspirate5-rev10 0 and 0, lumenpnp-mobo 0 and 0, cynthion 8 and 8 for
11 and 11, glasgow-revC3 24 and 26 for 30 and 28, sweep-v2.2 3 and 3, nrfmicro 0 and 0. The crate gives at 40
million nodes what the source gave, to the via. guide's curated boards with its own defaults, where `whole` is off
and no host is asked, routed from the release's source: 1150 of 1151 connections with 712 vias and the same 37556357
nodes as 0.0.18.
**Spokes** (the same runs; `board::thermal::census`):

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2348 to 2355 | 36, 49, 49, 49 | 154, 159, 170, 164 | 432 to 441 |
| guide 0.0.16, four orders (D110) | 2355 to 2363 | 2, 2, 2, 2 | 91, 82, 89, 100 | 442 to 461 |
| guide 0.0.17, four orders (D113) | 2361 to 2366 | 1, 1, 1, 3 | 97, 91, 88, 97 | 459 to 468 |
| guide 0.0.18, four orders (D114) | 2350 to 2367 | 1, 1, 1, 3 | 100, 94, 90, 95 | 458 to 467 |
| guide 0.0.19, four orders | 2372 to 2374 | 2, 3, 0, 0 | 73, 76, 75, 77 | 469 to 473 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2 against 1 to 2; corne-cherry 0 to 2 against 2 to 4; tinytapeout-demo 9 to 14 against 16 to 21; buspirate5-rev10 29 to 34 against 38 to 44; lumenpnp-mobo 0 against 2 to 3; cynthion 0 to 4 against 2; glasgow-revD1 23 to 28 against 85 to 96. With no neck: sweep-v2.2 0 against 0 to 2; tinytapeout-demo 0 against 0 to 3; lumenpnp-mobo 0 against 0 to 3; cynthion 0 to 3 against 0; glasgow-revD1 0 against 28 to 47. buspirate5-rev10, which D114 left at 33 to 43 short for 31 to 38 with 0.0.17, has 29 to 34 (its own routing: 29).
Taking the needless stubs out was not what had left more of its pads short (below, "Tried"); what has changed is
the grid its nets are negotiated on. tinytapeout-demo has more, 9 to 14 for 6 to 10, on the same grid.
**The targets.** The targets were the vias of lumenpnp-mobo, tinytapeout-demo and corne-cherry within 15 % of the native router's
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
**What cynthion and glasgow-revC3 leave open**, looked at connection by connection:
- *cynthion*, 11 to 12 with 0.0.18: three were the exposed pads of U8, U9 and U11 (above), and are joined now.
  The 8 left, in every run, are the Kelvin terminals of the four sense resistors R1, R2, R42 and R59, which are
  net ties: pads 2 and 3 of each, to pads 7 to 14 of U1. No track of 0.127 mm leaves such a pad at the board's
  clearance from the tie's pads 1 and 4 beside it, of another net (the native router's own word for the same
  eight, which it leaves too: "no legal way out of R59.2 for a 0.127mm track: pad R59.1"). Their own board
  routes them within that clearance: the DRC lets a tie's nets touch the tie's copper drawing and not its pads
  (ROUTER.md, the table's row for net ties). Not a router's to change.
- *glasgow-revC3*, 31 to 35 with 0.0.18 and 31 to 37 for the native router: every one ends at a ball of U30, a
  BGA of 121 balls at 0.8 mm. Its own board leaves the inner rows through 47 vias of 0.4 mm with a 0.2 mm hole
  between the balls, and tracks of 0.15 mm. guide makes 34 dog-bones there with the smallest via the rules
  allow, and says of what is left that the budget ended before a net taken out for another was routed again
  (most of them), that a ball is shut in by its neighbours' pads and routes, or that it found no way. With
  150 million nodes the board has 28 to 30 open, so it is not the minute alone: a plan for the whole array, as
  D108's first gap asked of the packages of fine pitch, is what neither router has.
**Tried and not kept** (guide's `docs/benchmarks.md` has each with its numbers):
- *The rounds' history forgotten before the pass for fewer vias, and the pass made up to four times*: the
  second saves 0 to 3 vias; with the others where they are a net has no better way.
- *The nets with something shared routed again whole*, every round or every fourth; *every net near a shared
  point routed again whole*, as the native router does it, in every round or in the first eight: no fewer vias
  at 150 million nodes on lumenpnp-mobo and buspirate5-rev10, and on corne-cherry 518 for 625 only at 300
  million nodes and 2.6 times the work.
- *Twice the pitch on two layers* (nrfmicro, above), *the first pitch with three quarters of the run on six*
  (glasgow-revD1 161 and 198 open for 125 and 135), *a negotiation at twice the pitch begun on a budget that
  holds two rounds of it* (glasgow-revD1 599 open over three orders at 40 million nodes for 441).
- *The end of a negotiation that was cut, at the first pitch too*: corne-cherry 95 open over three orders at
  40 million nodes for 79.
- *Tracks from pad to pad taken out where a fill joins the two*: another net's pour takes the room where the
  layer has one with a higher priority, and the fill that joined the pads is then another. On cynthion a strip
  of the ground's pour beside such a track went with it, and the adapter's check found the pad at its end open
  in four runs of 27.
- *Vias let into every heat pad*, shut in or not: corne-cherry's two packages have a ground pin beside their
  exposed pads, and with the room under them taken by vias the board had 1, 4 and 3 open at 150 million nodes
  for 2, 1 and 0.
- *The rooms of spokes told half a step round each point of the coarser grid*: buspirate5-rev10 33 to 36 pads
  short at 40 million nodes for 31, with a connection open in one order of three.
- *Dearer vias in guide's passes after its negotiation*, 2 mm of track for 1: corne-cherry 88 open over three
  orders at 40 million nodes for 79, glasgow-revC3 103 for 100, and as many vias on tinytapeout-demo.
- *The thermal census after the stubs went* (D114's question): with the needless stubs taken out by guide, by
  the adapter, or by neither, buspirate5-rev10 has 44, 45 and 45 pads short of two necks at 150 million nodes,
  lumenpnp-mobo 3 each, tinytapeout-demo 7, 8 and 8. Taking them out is not what leaves pads short.
**What got worse.** Against 0.0.18 (D114, another hour): *track* on the boards negotiated at twice the pitch,
where a via costs twice the track it did and the lines are twice as far apart: by the medians of the four runs
against the native router's, tinytapeout-demo 23 % over (14 % by D114's first runs), lumenpnp-mobo 20 % (17 %),
buspirate5-rev10 17 % (14 %), glasgow-revC3 12 % (6 %), glasgow-revD1 21 % (13 %); cynthion has less, 19 % (25 %),
and corne-cherry 5 % (8 %). *tinytapeout-demo*: one order of four with 7 open at 60 s where it had 4 at most
(0, 7, 1, 0 for 0, 4, 0, 4; medians 0.5 for 2), 0 and 3 at 150 million nodes for 0 and 2 (0 and 0 with 0.0.17),
and its pads short of two necks 9 to 14 for 6 to 10. *cynthion*: 0 to 3 pads with no neck for 1 to 3, two or three pins of J6 on the back in
two orders and none in the other two, where 0.0.18 had a pin of J3: all there are on the twelve boards (2, 3, 0, 0
for 1, 1, 1, 3). No board has more connections open by
its range but tinytapeout-demo's one order, and none at 40 million nodes.
**Gaps, for guide's next release**, most useful first:
1. *The vias that guide's passes add after a negotiation that was cut* (tinytapeout-demo): its negotiation does
   not end in a minute at either pitch, with 38 to 98 points shared when its share is over, and the passes then
   add 70 to 160 vias for the 15 to 22 connections it leaves them, of which cleanup takes 33 to 52 back. Dearer
   vias in those passes are no answer (above). What is left to try: the nets that the passes rip up routed again
   on the negotiation's grid, at its prices, and not by the passes' own search.
2. *The grounds of a board of two layers* (corne-cherry): 134 to 162 vias in its two grounds for the native
   router's 63 to 75, for as many pieces of pour. What the fill makes needless goes now; what stays is needed for the pieces as
   guide's routes leave them, each joined to the other layer by a via where the native router joins several by
   one. A plan of the pours' pieces as one net to join, piece to piece by the shortest way, and not pad by pad
   to the largest.
3. *A plan for a whole array of balls* (glasgow-revC3's U30), and *a track that leaves a net tie's pad within
   the tie* (cynthion's eight, a rule of the DRC's and of both routers).
4. *Track*: guide's is still longer than the native router's on every board of the table.
5. *glasgow-revD1*: its needless vias are not taken out, as a fill of the board takes 5.7 s and two are needed;
   and at 40 million nodes it has no negotiation.
6. *Rule areas with a routed track width* (tinytapeout-demo's +3V3 pins, what it has open), and *sweep-v2.2's
   time*: not touched.
**Default: unchanged.** By the ranges of four runs of each, with 0.0.19, guide is behind the native router in every run on no board: level on 5 (buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2); ahead in every run on 4 (buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 5 to 33; cynthion 8 against 28 to 29; glasgow-revD1 93 to 112 against 241 to 376); ranges that overlap on 3 (corne-cherry 0 to 2 against 0 to 4; tinytapeout-demo 0 to 7 against 2 to 15; glasgow-revC3 29 to 34 against 31 to 36; by their medians, corne-cherry 0.5 against 3; tinytapeout-demo 0.5 against 8.5; glasgow-revC3 31.5 against 33). `router: guide` stays the second opinion and the native router stays the default: the switch is the user's to make, and the evidence is this table, the spokes, the vias above and the budgets of nodes.
*Not measured:* the generated boards with 0.0.19; more than four runs of either router; KiCad's DRC on a board
guide routed; cadlab's oracles; cadlab's whole test suite, which is run at the merge.

### D116. guide 0.0.20 adopted: the ways out of an array of balls planned, the tracks between pads that a pour joins taken out by the fill, a shorter way looked for once the runs are over where they leave the time; the default unchanged (2026-11-18)
This round is for D115's gaps: the track, longer than the native router's on every board; the vias of
tinytapeout-demo and corne-cherry; glasgow-revC3's array of balls; cynthion's eight; tinytapeout-demo's worse
order. `guide = { version = "0.0.20", default-features = false, features = ["pcb"] }`; nothing else in
`Cargo.lock` moves. Measured with the crate from crates.io, on the fill of D112.
**Where guide's track was longer**, counted before anything was changed (`route_bench` with
`CADLAB_BENCH_VERBOSE=1` now tells the track outside the pads of its net, and with `CADLAB_BENCH_BY_NET=<dir>`
writes each result's nets and tracks; one run of each router at 60 s). Of the nets with no zone that both routers
close, 70 % on tinytapeout-demo and 64 % on lumenpnp-mobo are more than a tenth longer with guide, and 7 % and
3 % a twentieth shorter: no few nets carry it. Three things, each measured:
- *A net keeps the way round another that has moved since.* A negotiation leaves every route where the prices of
  its rounds put it (tinytapeout-demo's has 5233 mm after its first round, each net where it would go alone, and
  6145 mm when its share ends it), and the passes route a net round what is there then. guide's cleanup pulled
  each run of tracks tight where it lay and looked for fewer vias; it looked for no other way.
- *A net with a pour is joined pad to pad by tracks that the pour joins as well.* lumenpnp-mobo's ground: 402
  tracks and 448 mm for 20 vias after guide, 93 tracks and 124 mm for 21 vias after the native router, which
  plans on the pours. The nets with zones had 1301 mm for 813 on that board, 1184 for 779 on glasgow-revC3, 884
  for 666 on buspirate5-rev10.
- *A track of guide's goes to the middle of its pad*, and the native router's ends at the first point of its
  grid on the pad's copper. buspirate-rs232: 359 mm against 281, 28 % more, of which 292 against 272, 7 % more,
  is outside the pads of its net. That part is copper on copper, and stays: a track that ends at a pad's centre
  is what a board file is read by.
**What guide 0.0.20 does** (its `docs/benchmarks.md`, "PCB routing out of an array, and shorter after it"; its
`docs/architecture.md` has the decisions):
- *The second ring of an array of balls has its ways out before anything is routed*, in a run with a budget.
  glasgow-revC3's 29 to 34 open all ended at U30, 121 balls at 0.8 mm with room for one track between two. Its
  own board takes the first ring out on the top, the second between the balls of the first, and every ball
  further in through a via of 0.4 mm on the diagonal away from the middle, 46 of them. guide gave the balls
  further in their dog-bones towards the middle and the second ring nothing: a net that came first went along
  the first ring, the shortest way across, and closed both ways out of every ball it passed. Now each ball of the
  second ring has a track to the middle of the four balls beside it and straight out past the first ring, as
  copper of its net, for a net with no zone and no more than eight pads; and the dog-bones are on the side away
  from the middle.
- *`RouteOptions::shorten`*, off by default in guide: in cleanup, each run of a net's tracks from one pad or
  branch to the next that costs more than the straight way between its ends is taken out, the longest way round
  first, and searched for again between the two pieces that this leaves the net in, every other route where it
  is; kept if it costs less, a via at three times its routing price. The routes that a run was given are looked
  at as well, but those of a pair's nets and of a net with a length to keep, and what a run that went another
  way cuts off a fill that guide plans on is joined to it again.
- *`pcb::needless_between`*: as `needless` (D115), and every track that the pads of its net are joined without,
  whatever it ends at, the longest first. `needless` leaves a track from pad to pad because the fill without it
  is another; this one is for a host that makes that fill and looks at it.
- *At its first pitch, the price of a point that two nets claim stops at thirty times a step*, where it went to
  a thousand, in the negotiation for every net: what is still shared is its history's to price. At twice the
  pitch, on more than two layers, it is as it was.
**What the adapter does with it** (`router::guide`; ROUTER.md, "guide as a backend"; BOARD.md, "What a router
must be told"):
- *The tracks between pads that a pour joins, by the fill and by the reliefs.* What the fill of the routed
  board makes needless now goes in two steps. First the vias and what led to them, as D115 had it. Then, with
  the time of two fills and three counts, the tracks that `needless_between` tells: the joints of the pads of
  the nets with pours are counted (`thermal::joints_of`, new: `joints` for the nets asked for), the tracks are
  taken out together and the board is filled; a pad that this leaves short of its spokes where it was not, or
  with fewer necks than it had and fewer than two, has what was taken from its net within 1.5 mm back, as a
  connection that is open now has what was taken within 3 mm, and the board is filled once more; kept if it has
  no more open and no pad is worse off. A track to a pad is a neck of it, and the pour that takes its place is
  not always one: two to five pads a board have their tracks back. The later runs' deadline leaves
  a fill and one verification's time more for it.
- *A run for shorter ways once the others are over, where they leave the time.* guide has the routes back once
  more, as a run that moves routes has them, with nothing to route (a net with a connection open keeps its
  routes as copper of the board unless it has a zone; so do a pair's nets and the nets of a length group), no
  right to take a net out, and `shorten`. Verified as any run. Where a connection is open that was not (a track
  that went another way may cut a pour where the old one did not, and guide's own count of its fills does not
  always tell: two pads of cynthion's +2V5 under its array, in every run tried), the nets with new copper
  within 1.5 mm of an end of it have
  the routes they had back, as have those that the check then takes copper out of, and the rest is verified
  again, three times at most; kept if the check takes nothing out and the board has no more open. No time is
  kept for it (below).
- *`Spare` is gone*: what each run told needless by its own fill stays on the board until the runs are over, as
  before, and is told again by the fill of the board as it is then; nothing read the list. And the pass of D108
  for the stubs that are left is begun only with the time of its two fills by what a fill takes: by the first
  run's verification alone it began at 57.7 s on tinytapeout-demo in one order, after a first step that had kept
  nothing, and the call came back at 60.5 s.
**Measured** (ROUTER.md, "Native router and guide 0.0.20, D116"). With the crate from crates.io, four runs of
each router at 60 s, in the same half hour, two runs of each at a time: the native router with seeds 1 to 4 on
three threads, guide in four orders of its nets with three threads for its negotiation and one for the rest.
The twelve boards: 102 to 139 open for guide (median 117.5) against 335 to 376 for the native router (340);
the eleven without glasgow-revD1 18 to 23 (19) against 85 to 102 (93). 0 DRC errors other than unrouted in the
96 runs, no call of guide over 59.5 s (the native router's longest 62.9 s). By board, guide against the native
router: level on buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2; ahead in every
run on buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 5 to 13; cynthion 8 against 28 to 29;
glasgow-revC3 5 to 7 against 31 to 37; glasgow-revD1 84 to 120 against 238 to 289; ranges that overlap on
corne-cherry 0 to 2 against 0 to 4 and tinytapeout-demo 0 to 7 against 2 to 15; behind in every run on none.
Vias, the range of the four runs: corne-cherry 473 to 525 against 445 to 505, tinytapeout-demo 327 to 462 against
238 to 290, buspirate5-rev10 296 to 326 against 291 to 314, lumenpnp-mobo 331 to 348 against 339 to 348, cynthion
418 to 423 against 469 to 482, glasgow-revC3 399 to 453 against 415 to 439, glasgow-revD1 1067 to 1139 against
866 to 932, sweep-v2.2 18 to 24 against 21 to 28, nrfmicro 42 to 51 against 46 to 51. By the medians:
corne-cherry 502.5 against 464.5 (+8 %), tinytapeout-demo 390.5 against 261 (+50 %), buspirate5-rev10 313 against
308.5 (+1 %), lumenpnp-mobo 336 against 341 (-1 %), cynthion 423 against 478 (-12 %), glasgow-revC3 403 against
421 (-4 %), glasgow-revD1 1123.5 against 887.5 (+27 %). Track, the medians, and in brackets the same for the
track outside the pads of its net: buspirate-flash-sop 494 mm against 461 (+7 %; 431 against 431, 0 %),
buspirate-rs232 358 against 281 (+28 %; 291 against 272, +7 %), lumenpnp-ringlight 198 against 177 (+12 %; 155
against 159, -3 %), nrfmicro 536 against 450 (+19 %; 450 against 414, +9 %), sweep-v2.2 2202 against 2159 (+2 %;
2121 against 2122), corne-cherry 7595 against 8212 (-8 %; 7322 against 8027, -9 %), tinytapeout-demo 6035
against 4924 (+23 %; 5669 against 4700, +21 %), buspirate5-rev10 3247 against 3088 (+5 %; 2929 against 2870,
+2 %), lumenpnp-mobo 6167 against 5511 (+12 %; 5555 against 5220, +6 %), cynthion 3758 against 3184 (+18 %; 3263
against 2981, +9 %), glasgow-revC3 4486 against 3478 (+29 %; 3998 against 3216, +24 %, for some 28 connections
more), glasgow-revD1 9392 against 7708 (+22 %; 8321 against 7212, +15 %, for some 150 more). Time: nrfmicro 3 to
48 s against 2 to 12 s, corne-cherry 25 to 59 s against 22 to 38 s, buspirate5-rev10 28 to 39 s against 30 to
41 s, lumenpnp-mobo 57 to 59 s against 61 to 62 s.
What each step did in those runs. *The tracks between pads*: lumenpnp-mobo 575 to 620 tracks and 520 to 552 mm
in every order, corne-cherry 803 to 840 and 310 to 384 mm, buspirate5-rev10 391 to 418 and 226 to 250 mm,
tinytapeout-demo 280 and 331 tracks, 201 and 223 mm, in two orders of four, glasgow-revC3 397 and 135 mm in one;
on cynthion and in the other orders of those two the runs left no time for it, and on glasgow-revD1, 5.7 s a
fill, it is not made. *The run for shorter ways*: made on the boards whose runs end before the minute,
buspirate5-rev10 75 to 188 mm less with 11, 6 and 17 vias fewer and one more, tinytapeout-demo 322 mm and 15
vias in the one order whose first run ended early, corne-cherry 25 vias fewer for 65 mm more in one order,
nrfmicro 0.4 to 6.4 mm, the three small boards 0 to 8 mm with three or four nets left as they were on
lumenpnp-ringlight; not on lumenpnp-mobo, cynthion, sweep-v2.2 or the two glasgows.
At a budget of 40 million nodes, three orders, the nine boards a budget cuts, beside 0.0.19: 620 open for 675
(nrfmicro 0 for 0, sweep-v2.2 9 for 9, corne-cherry 79 for 79, tinytapeout-demo 30 for 30, buspirate5-rev10 0
for 0, lumenpnp-mobo 0 for 0, cynthion 24 for 24, glasgow-revC3 41 for 100, glasgow-revD1 437 for 433).
At 150 million, two orders, beside 0.0.19: corne-cherry 1 and 0 for 2 and 1, tinytapeout-demo 0 and 3 for 0 and
3, buspirate5-rev10 0 and 0, lumenpnp-mobo 0 and 0, cynthion 8 and 8, glasgow-revC3 5 and 9 for 24 and 26,
sweep-v2.2 3 and 3, nrfmicro 0 and 0. There both steps are made on every board that the budget does not stop,
and the track is, against the native router's median at a minute: tinytapeout-demo 5690 and 5432 mm (+16 % and
+10 %, with 358 and 341 vias), lumenpnp-mobo 6064 and 6013 mm (+10 % and +9 %), cynthion 3558 and 3471 mm
(+12 % and +9 %). The crate gives at 40 million nodes what the source gave. guide's curated boards with its own defaults,
where `whole` and `shorten` are off and no host is asked, routed from the release's source: 1150 of 1151
connections with 712 vias and the same 37556357 nodes as 0.0.19.
**Spokes** (the same runs; `board::thermal::census`):

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2348 to 2355 | 36, 49, 38, 38 | 154, 159, 162, 157 | 432 to 441 |
| guide 0.0.16, four orders (D110) | 2355 to 2363 | 2, 2, 2, 2 | 91, 82, 89, 100 | 442 to 461 |
| guide 0.0.17, four orders (D113) | 2361 to 2366 | 1, 1, 1, 3 | 97, 91, 88, 97 | 459 to 468 |
| guide 0.0.18, four orders (D114) | 2350 to 2367 | 1, 1, 1, 3 | 100, 94, 90, 95 | 458 to 467 |
| guide 0.0.19, four orders (D115) | 2372 to 2374 | 2, 3, 0, 0 | 73, 76, 75, 77 | 469 to 473 |
| guide 0.0.20, four orders | 2371 to 2375 | 2, 3, 0, 2 | 64, 71, 75, 75 | 468 to 473 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2 against 1 to 2; corne-cherry 1 to 3 against 2 to 4; tinytapeout-demo 8 to 12 against 16 to 21; buspirate5-rev10 26 to 32 against 38 to 44; lumenpnp-mobo 0 against 2 to 3; cynthion 2 to 4 against 2; glasgow-revD1 23 to 28 against 85 to 96. With no neck: sweep-v2.2 0 against 0 to 2; tinytapeout-demo 0 against 0 to 3; lumenpnp-mobo 0 against 0 to 3; cynthion 0 to 3 against 0; glasgow-revD1 0 against 28 to 47.
Taking the tracks between pads out has left no more pads short: 64 to 75 for 73 to 77, with lumenpnp-mobo, which
loses the most tracks, at none. The pads with no neck are cynthion's, two or three pins of J6 in three orders
where 0.0.19 had them in two.
**The targets.** *Track within 10 % of the native router's median on every board*: met on four,
buspirate-flash-sop (+7 %), sweep-v2.2 (+2 %), corne-cherry (-8 %) and buspirate5-rev10 (+5 %, for +17 % with
0.0.19); not on eight. On three of those the track outside the pads is within it and what is over is copper on
the net's own pads (buspirate-rs232 +7 %, lumenpnp-ringlight -3 %, nrfmicro +9 %); so it is on lumenpnp-mobo
(+12 %, for +20 %; +6 % outside) and cynthion (+18 %; +9 %); tinytapeout-demo (+23 %, as it was) and the two
glasgows (+29 % and +22 %, with more routed) are over by either count. *Vias within 15 % on tinytapeout-demo and
corne-cherry*: corne-cherry meets it, 473 to 525 against 445 to 505, medians 502.5 against 464.5 (566.5 with
0.0.19), by the price that stops at thirty and by 5 to 22 needless vias and 803 to 840 tracks taken out;
tinytapeout-demo does not, 327 to 462 against 238 to 290 (362 to 462 with 0.0.19). *glasgow-revC3 clearly below
the native router's range*: met, 5 to 7 open against 31 to 37 (29 to 34 with 0.0.19), and 41 for 100 at 40
million nodes. *cynthion's eight* and *tinytapeout-demo's worse order*: diagnosed below, neither changed.
**cynthion's eight**, diagnosed on its own board and in cadlab (D115 put them down to the DRC's rule for net
ties). They are pads 2 and 3 of R1, R2, R42 and R59, `R_0603_1608Metric_Current_Sense`, with
`net_tie_pad_groups "1, 2" "3, 4"`. Pad 2, a rectangle of 0.50 by 0.15 mm on the back alone, lies wholly inside
pad 1, 0.95 by 0.80 mm of another net, its inner edge flush with pad 1's; pad 3 in pad 4 the same way; the
footprint has no copper drawing. Any copper that leaves pad 2 is at no distance from pad 1, and no via fits in
0.15 mm (the board's smallest is 0.41 mm). The project's own tracks, 0.127 mm, start at the sense pad's centre
and cross the shared edge into the 0.85 mm between the two force pads, to a via outside the footprint: inside
pad 1's outline they are within pad 2's own rectangle, and for the first 0.13 mm outside it they are closer to
pad 1 than the board's 0.127 mm. cadlab lets the items of one tie group touch each other (`board::tied`: both
carry the same group, or one is the footprint's copper drawing between them) and nothing else: a track carries
no group, so the sense track against pad 1 is a short and a clearance error, in the DRC and for both routers.
That is as KiCad's DRC has it by the record here: an exemption for tracks near tied pads routed all eight and
was reverted when KiCad's DRC gave `shorting_items` on those tracks, the project's own among them (D72,
"Residuals"; `tests/corpus/projects.toml`, cynthion's `known`: KiCad reports 4 of the 8 crossings on the
original board and 7 on the re-export). So there is no
change here that is clearly right. A narrower exemption is conceivable, a track of a tie group's net whose
overlap with the group's other pad lies inside its own net's pad of that group, with no clearance asked at that
edge: it would differ from KiCad on the crossings that KiCad reports, and wants the oracle run that this round
may not make. Left as it is: both routers report the eight, with the pad that shuts each in.
**tinytapeout-demo's worse order**, looked at round by round (guide's negotiation, one run each of two orders).
The points shared fall from 3335 after the first round to 339 and 359 after the sixth and 112 and 156 after the
twelfth, by which the price is nearly at its most; from there to the end of its share, forty rounds on, they go
between 54 and 230 with 20 to 44 nets in a round, while the vias rise from 370 and 382 to 442 and 453 and the
track from 5700 mm to 6145 and 6333 mm. It
does not end because those nets have no way that none of the others wants at the lines of its grid, a track and
its clearance apart, and each round moves the same few conflicts on. Which order of the four then has several
open is the one whose first run leaves no time for a run that moves routes: 0, 7, 1 and 0 with 0.0.19, and
0, 7, 0 and 0 with 0.0.20, the same second order. Not solved in this round: what the rounds after the twelfth cost is known, and ending the negotiation
there, for the passes to have the time, was not tried.
**Tried and not kept** (guide's `docs/benchmarks.md` has those that are guide's, each with its numbers):
- In guide: *ways out for the first ring too*, straight out of each ball (glasgow-revC3 17, 14, 17 and 11 open in
  four runs for 16 and 13); *the ways out of the second ring for every net* (glasgow-revD1, 256 balls, 426 open
  over four orders for 388: a ball of a supply goes to its plane by a via where it is); *the price at thirty at
  twice the pitch too* (glasgow-revC3 33 to 36 open over four orders in three comparisons for 22 to 27,
  tinytapeout-demo 6 for 4, with some 3 % less track and as many vias); *a via at its routing price in a run
  that is searched for again* (lumenpnp-mobo 13 to 15 vias more, a millimetre of track saved each); *the history
  of a shared point at twice its step with the price at a hundred* (tinytapeout-demo 372 and 383 vias in two
  orders for 321 and 318).
- *Shorter ways at the end of each run of guide's*, the first among them (`shorten` set for every run).
  nrfmicro's last connection, which a run that moves routes closes in 19 s in its first order, had no way in
  five such runs once the first run's routes were made shorter, and the board ended at 58 s with it open;
  lumenpnp-mobo's first run ended 4 to 7 s later and left no time for the run that closes its last connection
  (2, 2, 0 and 1 open for none); glasgow-revD1 had 471 open over four orders for 428, with 8800 to 8962 mm of
  track for 9064 to 9439.
- *Time kept for the run for shorter ways*: 6 % of the budget for its searches and four verifications'. The
  boards that a minute does not finish then had no time for a run that moves routes: over four orders
  tinytapeout-demo 13 and 16 open in two such configurations for 6 without, lumenpnp-mobo 4 and 5 for none,
  glasgow-revC3 36 for 22, for 201 to 372 mm of tinytapeout-demo's track, 135 to 141 mm of glasgow-revC3's and
  37 to 93 mm of lumenpnp-mobo's. So it is made where the runs end early, and by a larger budget.
- *The vias and the tracks between pads taken out in one step*, one fill for both and the second for what a pad
  or a connection needs back: tinytapeout-demo had nothing taken out in three orders of four, where the second
  fill did not mend what the first made worse, and 400 to 462 vias. Hence two steps, the first as D115 had it.
- *The second step begun with the time of one fill*: on cynthion it ended before its second in three orders of
  four, 2.3 s for nothing.
- *cadlab's fill given to guide in the run for shorter ways*, as a run that moves routes has it: up to seven
  fills in that run on lumenpnp-mobo, 12 s for 65 mm, and cynthion's two pads were cut all the same.
- *The old routes back for every net with new copper within 3 mm of the box of a connection that the shorter
  ways open*: the check then took 53 and 221 pieces of copper out on cynthion and tinytapeout-demo, old routes
  beside new ones. Hence the nets within 1.5 mm of its ends, and then those that the check names.
- *A larger share of a run's time for guide's cleanup, for the shorter ways in it*: what the first run took
  longer, the runs after it had less.
**What got worse.** Against 0.0.19 (D115, another hour). *tinytapeout-demo's vias by the median*: 390.5 for
369, in a range that is no wider (327 to 462 for 362 to 462), and its track is where it was, +23 %: the two
orders that end late have no second step and no run for shorter ways. *glasgow-revD1*: 84 to 120 open for 93 to
112 (medians 96.5 for 105), one order with more than any before, and 437 for 433 at 40 million nodes: its array
has its ways out too, which is all that differs for it and gained nothing where it was measured alone (395
open over four orders for 392). *glasgow-revC3's copper*: 399 to 453 vias for 330 to 359 and 4486 mm for 3894,
+29 % against the native router's for +12 %, with some 25 connections more made through its array. *Time*: lumenpnp-mobo 57 to 59 s for 53 to 56 s and the boards that route in under a second
0.7 to 1.1 s for 0.4 to 0.8 s, by the second step and the run for shorter ways; nrfmicro's fourth order 48 s
for 28 s, with the price that stops at thirty (26 s at most over four orders with it at a thousand, the rest
the same); and the later runs' deadline keeps a fill and a verification's time more than it did. *cynthion's pads with no neck*: 0 to 3 as before, in three orders for
two. No board has more connections open by its range but glasgow-revD1's one order.
**Gaps, for guide's next release**, most useful first:
1. *Time for what comes after routing, on the boards that a minute does not finish.* The second step is worth
   4 % of tinytapeout-demo's track and some 7 % of cynthion's, the run for shorter ways 5 % of
   tinytapeout-demo's and 3 % of glasgow-revC3's, and they are made there in a few orders or in none. Two ways to the time, neither tried: guide's searches for fewer vias
   once, in the last run, where every run of the adapter has them now (7 to 8 s of tinytapeout-demo's first
   run); and a verification of the nets that a run changed, where each run's is the whole board's (0.9 to 3.5 s
   a time).
2. *tinytapeout-demo's negotiation*, which gains nothing after its twelfth round and has forty more: an end
   where it stops gaining, with the time to the passes. Its vias (327 to 462 against 238 to 290) are made there.
3. *A pour cut by a shorter way that guide's count does not tell* (cynthion's +2V5 under its array): told by
   cadlab's check after the run, at a verification more and thirteen nets left as they were. Why guide's count
   misses it was not looked into.
4. *glasgow-revD1*: its ways out gain nothing, its needless vias stay, as a fill of the board takes 5.7 s
   (cadlab's `zone` fill itself was not profiled in this round), and at 40 million nodes it has no negotiation.
5. *What is left of glasgow-revC3's array* (5 to 7) and *of corne-cherry* (0 to 2), *rule areas with a routed
   track width*, *sweep-v2.2's time*: not touched.
6. *A track that leaves a net tie's pad within the tie* (cynthion's eight): a rule to settle with KiCad's DRC
   beside it, above.
**Default: unchanged.** By the ranges of four runs of each, with 0.0.20, guide is behind the native router in every run on no board: level on 5 (buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2); ahead in every run on 5 (buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 5 to 13; cynthion 8 against 28 to 29; glasgow-revC3 5 to 7 against 31 to 37; glasgow-revD1 84 to 120 against 238 to 289); ranges that overlap on 2 (corne-cherry 0 to 2 against 0 to 4; tinytapeout-demo 0 to 7 against 2 to 15; by their medians, corne-cherry 1 against 3; tinytapeout-demo 0 against 9). `router: guide` stays the second opinion and the native router stays the default: the switch is the user's to make, and the evidence is this table, the spokes, the vias and the track above and the budgets of nodes.
*Not measured:* the generated boards with 0.0.20; more than four runs of either router; KiCad's DRC on a board
guide routed; cadlab's oracles; cadlab's whole test suite, which is run at the merge.
### D117. guide 0.0.21 adopted: the reliefs of a fill kept from one fill to the next, no fill at a run's end, the runs that move routes ended after two that gain nothing, the escapes of a run that moves no route planned round its routes; the default unchanged (2026-11-18)
This round is for D116's first gap, the time for what comes after routing, and for what that time is to buy:
the track and the vias, glasgow-revD1, the pour that a shorter way was thought to cut, cynthion's eight.
`guide = { version = "0.0.21", default-features = false, features = ["pcb"] }`; nothing else in `Cargo.lock`
moves. Measured with the crate from crates.io.
**Where the time went**, counted before anything was changed (the adapter's fills and checks timed, one run of
guide 0.0.20 at 60 s on two boards, two at a time). *A verification is a fill*: glasgow-revD1's two take 3.26 s
and 3.30 s, of which the fill 3.00 s each and the check of the filled board 0.26 s and 0.29 s; tinytapeout-demo's
three 0.88 to 0.94 s, of which the fill 0.76 to 0.80 s. Of the boards as their projects route them the check
after the fill takes 0.37 s at most (glasgow-revD1; lumenpnp-mobo 0.23 s, tinytapeout-demo 0.14 s). So a check of
what a run changed alone was not written: what it would save is a tenth of a verification, and the fill is the
rest. *The fills of a run*: tinytapeout-demo's 57 s have nine that guide asks for, three of them at the end of a
run (seven made, 0.79 to 0.98 s each, two answered from the fill kept), three verifications' and those of the two
steps that take needless copper out, 4.3 s: some 13 s; and guide's cleanup 7.0 s, 4.4 s and 5.4 s in its three
runs that have one. *glasgow-revD1's minute*: a first run to its
share, 42 s, a verification, a second run that begins with 3.1 s of guide's own planning and routes for 4.6 s,
a verification, and the report at 57.6 s.
**cadlab's fill, profiled** (`board::zones`; BOARD.md, "Zone fill", has the table). Of glasgow-revD1's fill as its
project routes it, the spokes of the relieved pads take 3.0 s of the 3.7 s that its steps take over all its
layers' threads: 338 relieved pads on its busiest layer, each spoke that does not fit whole trimmed with four
booleans and two offsets, every direction of the relief tried for a pad short of its spokes. Three changes,
none of which changes a fill:
- *A pad's relief is a function of what is near it* (`PadKey`, `pad_spokes`): the pad, its relief, the polygons
  of the allowed area and of the pour that meet the box of its spokes, and the same-net pads of the relief whose
  windows meet it. Made a pad at a time, the pad's gap grown once for all its spokes where each spoke grew it
  again, side by side with the `parallel` feature (the pads of a layer over rayon's threads, put together in
  the order of the pads).
- *`fill_zones` keeps each relief by what it was made from* (`SpokeMemo`), compared in full, and a later fill
  takes it from there where all of that is as it was: a fill after a few tracks moved makes the reliefs near
  them again and no other. In two ages, the older dropped when the newer holds four million points; forgotten
  with the fills by `clear_fill_cache`; `fill_zones_uncached` uses none of it.
- *The same fill, point for point*, three ways: the fills of the twelve corpus boards as routed, with every
  seventh track and fifth via out, every third and second, and with none, have the hashes that the code before
  gave them (48 fills of boards); `tests/zone_refill.rs` compares `fill_zones` with `fill_zones_uncached` on
  every corpus board in eight states, one after another (as routed, its last 1, 5 and 40 tracks out, tracks
  and vias out all over down to none, as routed again), in release with `CADLAB_CORPUS_DIR`; and
  `reliefs_reused_give_the_fill_made_from_scratch` does on a grid of pads with rows in each other's gaps and
  tracks that come and go beside them. `zone_fills_match_reference` and `large_zone_fills_match_reference`
  pass as before.
Times of a fill, release, three threads, the best of two, before and now: glasgow-revD1 as routed 3239 ms and
1486 ms, and 872 ms again with its last five tracks out; with no track 8174 ms and 2741 ms; lumenpnp-mobo 1318,
809 and 593 ms; buspirate5-rev10 725, 428 and 289 ms; tinytapeout-demo 597, 482 and 375 ms; cynthion 775, 691
and 736 ms, corne-cherry 362, 367 and 337 ms: boards of many small zones have their time in the booleans of
each, which are made as before. Behind the adapter at 60 s glasgow-revD1's verification takes 1.7 s for 3.3 s
and tinytapeout-demo's 0.6 to 0.7 s for 0.9 s. *Not done*: whole zone layers kept by their inputs (the layers
of a board fill side by side, so a fill takes what its slowest layer takes, and that is the one a router
changes), and the booleans of a layer made for the changed places alone, which no test here could have shown
to give the same points.
**What the adapter does with it** (`router::guide`; ROUTER.md, "guide as a backend"):
- *No fill at a run's end.* guide asks its host for a fill as each run ends, to tell the stubs that the fill
  makes needless; since D114 the adapter keeps those on the board until its runs are over and tells them again
  by the fill of the board as it is then. The fill bought nothing, and is refused: one fill a run.
- *Two runs that move routes and say that the board has no room end them* (`MOVE_STALE`): runs that end on
  their own with no fewer connections open, every net still open one whose own search found no way
  (`Routed::failed`). A board whose open connections no order closes had the same asked of it until the time
  was gone: cynthion's eight had three such runs, and its run for shorter ways 1.5 s for its searches where
  after two it has 8.6 s. Counted by the gain alone, as it was first written and measured, nrfmicro's last
  connection stayed open in one order of four (0, 0, 0 and 1 for none): there the fourth run closes it after
  two that gained nothing, each with a net open that rip-up had taken out, which is no such word.
- *A net that the run for shorter ways leaves open has its old routes back*, with those that have new copper
  beside what opened: a net that guide took back and could not put back whole has no new copper to tell it by,
  and was never among them.
- guide's searches for fewer vias stay in every run (below).
**The pour that a shorter way cut was no pour** (D116's third gap; guide 0.0.21, its `docs/benchmarks.md`, "PCB
routing from routes, with none moved"). cynthion's run for shorter ways came back in every run of it with two
pads of +2V5 open under its array, and was not kept. Followed on the board at a budget of 150 million nodes,
which repeats: the three tracks and the via that joined C24.1 to IC1.G11 and IC1.L7, the via between four balls
at (130.4, -92.8), were not in what guide returned, and no other net's new track was near. The via was cut as
the run began. A run from routes planned its escapes on the board without the routes, as a first run does, and
put the routes on it afterwards, each cut where it no longer fitted: IC1.H11, a ball of +3V3 beside that place,
had its dog-bone planned there again, as the first run had planned it, not used it, and given the place to
+2V5 since. In the runs that move routes the same happened, and cost time and no connection: 74 to 78 of
cynthion's 334 nets routed again in each of them, 16 to 22 moved aside. guide 0.0.21: a run with `rip_ups` at
none, which the run for shorter ways is, plans its escapes on the board with the routes it was given on it.
cynthion at 150 million nodes, the same adapter: the run for shorter ways routes 4 nets again for 7 (2 of them
with no way), leaves 8 connections open for 11, is kept (97 mm less), and the board ends with 424 vias and
3449 mm for 426 and 3558 mm. Not in the runs that move routes: planned round the routes there too, they route
12 nets again and close less, glasgow-revC3 7, 9, 7 and 6 open over four orders at 150 million nodes for 6, 9,
4 and 3 (tinytapeout-demo none and corne-cherry 1, 0, 2 and 0 either way). An escape planned over another
net's route is how the ball it is for comes out, on a board that is full.
**Measured** (ROUTER.md, "Native router and guide 0.0.21, D117"). With the crate from crates.io, four runs of
each router at 60 s, in the same quarter of an hour, two runs of each at a time: the native router with seeds 1
to 4 on three threads, guide in four orders of its nets with three threads for its negotiation and one for the
rest. The native router is not the one of D116's table: it has the same fill, and glasgow-revD1, where a fill
took it 3 s a time, is another board for it, 99 to 169 open for 238 to 289, with 1 to 39 pads with no neck for
28 to 47. The twelve boards: 102 to 140 open for guide (median 120) against 178 to 268 (205.5); the eleven
without glasgow-revD1 18 to 21 (19.5) against 79 to 99 (94). 0 DRC errors other than unrouted in the 96 runs,
no call of guide over 59.4 s (the native router's longest 63.1 s). By board, guide against the native router:
level on buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro, sweep-v2.2; ahead in every run on
buspirate5-rev10 0 against 2 to 12, lumenpnp-mobo 0 against 1 to 24, cynthion 8 against 28 to 29, glasgow-revC3
4 to 7 against 31 to 36; ranges that overlap on corne-cherry 0 to 2 against 0 to 4, tinytapeout-demo 0, 6, 0
and 1 against 2 to 14, and now glasgow-revD1, 84 to 120 against 99 to 169 (medians 100 against 111.5); behind
in every run on none. Vias by the medians: tinytapeout-demo 381.5 against 261 (+46 %, for +50 %), corne-cherry
502.5 against 464.5 (+8 %), buspirate5-rev10 312 against 308.5, lumenpnp-mobo 335 against 341.5, cynthion 421
against 478, glasgow-revC3 405.5 against 416, glasgow-revD1 1016 against 1084 (for 1123.5 with 0.0.20: its
needless vias go now, 99 to 106 in every order). Track by the medians, and in brackets outside the pads of its
net: buspirate-flash-sop +7 % (0 %), buspirate-rs232 +28 % (+7 %), lumenpnp-ringlight +12 % (-3 %), nrfmicro
+18 % (+8 %), sweep-v2.2 +2 % (0 %), corne-cherry -8 % (-9 %), tinytapeout-demo 5922 mm against 4928, +20 %
(+18 %), buspirate5-rev10 +5 % (+2 %), lumenpnp-mobo 6070 against 5468, +11 % (+6 %), cynthion 3617 against
3184, +14 % (+7 %), glasgow-revC3 4502 against 3462, +30 % (+25 %), glasgow-revD1 9265 against 9147, +1 % (-4 %,
against a native router that routes some 130 connections more than it did). Time: lumenpnp-mobo 51 to 57 s for
57 to 59 s, buspirate5-rev10 23 to 35 s for 28 to 39 s.
What each step did in those runs, beside D116's. *The run for shorter ways* is begun in 28 runs of 48 for 23
and kept in 27: lumenpnp-mobo in every order (72 to 127 mm; in none with 0.0.20), cynthion in two (76 and
14 mm; none), buspirate5-rev10 in every order (78 to 200 mm), nrfmicro and the small boards as before,
corne-cherry in one, tinytapeout-demo in one and not kept there; not on sweep-v2.2 or the glasgows. *The vias
that the fill makes needless* go in every order of every board that has any, glasgow-revD1 among them. *The
tracks between pads*: lumenpnp-mobo, corne-cherry and buspirate5-rev10 in every order, cynthion in two (660
and 683 tracks, 277 mm; none with 0.0.20), tinytapeout-demo in two (343 and 346 tracks, 240 and 243 mm),
glasgow-revC3 in one, glasgow-revD1 in none. So the steps are not made on every board at a minute: the boards
whose first run ends at its share, 42 s (tinytapeout-demo and the glasgows, in every order here), have
what the runs after it leave, as before.
At a budget of 40 million nodes, three orders, the nine boards a budget cuts: 620 open, board by board what
0.0.20 had (glasgow-revC3 41, glasgow-revD1 437, tinytapeout-demo 30, corne-cherry 79, cynthion 24, sweep-v2.2
9, the others none). At 150 million, two orders: as 0.0.20 in what is open (corne-cherry 1 and 0,
tinytapeout-demo 0 and 3, cynthion 8 and 8, glasgow-revC3 5 and 9, sweep-v2.2 3 and 3, the others none), with
cynthion's track at 3449 and 3398 mm for 3558 and 3471 (+8 % and +7 % on the native router's median at a
minute) and tinytapeout-demo's 5689 and 5432 mm, lumenpnp-mobo's 6064 and 6013 mm as they were.
**Spokes** (the same runs; `board::thermal::census`):

| | pads in pours | no neck | short of two necks (`drc.starved_thermal`) | solid solder pads |
|---|---|---|---|---|
| as routed by their projects (D112) | | 2 | 116 | |
| native router, seeds 1 to 4 | 2350 to 2356 | 47, 7, 11, 2 | 159, 131, 146, 134 | 434 to 444 |
| guide 0.0.20, four orders (D116) | 2371 to 2375 | 2, 3, 0, 2 | 64, 71, 75, 75 | 468 to 473 |
| guide 0.0.21, four orders | 2371 to 2375 | 2, 3, 0, 2 | 70, 76, 75, 73 | 468 to 473 |

Short of two necks by board, guide against the native router: buspirate-rs232 1 against 1; sweep-v2.2 0 to 2
against 1 to 2; corne-cherry 1 to 3 against 2 to 4; tinytapeout-demo 11 to 13 against 16 to 21; buspirate5-rev10
25 to 32 against 38 to 44; lumenpnp-mobo 0 against 2 to 3; cynthion 2 to 5 against 2; glasgow-revD1 24 to 28
against 66 to 90. With no neck: cynthion 0 to 3, the other boards none, as with 0.0.20. The pads short of two
are 70 to 76 for 64 to 75: tinytapeout-demo has 11 to 13 for 8 to 12 and cynthion 2 to 5 for 2 to 4, the other
boards what they had. That is worse by a few pads, on the two boards whose results differ most from one run to
the next, and it was not followed to a cause.
**The targets.** *The steps after routing on every board at a minute*: not met. They are made on more
(lumenpnp-mobo's and cynthion's runs for shorter ways, cynthion's tracks between pads, glasgow-revD1's needless
vias), and not where the first run takes its whole share. *Track within 10 % of the native router's median on
every board*: met on four as before (buspirate-flash-sop, sweep-v2.2, corne-cherry, buspirate5-rev10) and on
glasgow-revD1 (+1 %, by a native router that routes more of it); lumenpnp-mobo +11 % for +12 %, cynthion +14 %
for +18 %, nrfmicro +18 % for +19 %, tinytapeout-demo +20 % for +23 %, glasgow-revC3 +30 % for +29 %,
buspirate-rs232 and lumenpnp-ringlight as they were. Counted outside the pads, within it on ten boards of
twelve; tinytapeout-demo (+18 %) and glasgow-revC3 (+25 %) are over by either count. *Vias within 15 % on
tinytapeout-demo*: not met, 372 to 427 against 243 to 290, +46 % by the medians. *glasgow-revD1 clearly lower,
its spread down*: not met, 84 to 120 for 84 to 120 (below). *The pour that a shorter way cut*: found and
mended in guide. *cynthion's net ties*: nothing changed (below).
**glasgow-revD1**, diagnosed. Its minute is its first run's share: the run is stopped at 42 s with 119 to 123
connections open on its own board, the second run closes 14 to 17 on the board with those routes fixed, and
no run that moves routes is ever begun. Of the 105 left by one order, 62 end at U8, its array of 256 balls,
14 at U2, 12 at J6, 11 at U7, and 101 are told as nets that had their turn and were without a way when the
run stopped. With three minutes the same order has 23 open, 20 of them at U8, and with five minutes 14, all at
U8: the first run then ends on its own at 126 s and 210 s. So what a minute leaves open is the time, and what
no time closes is the array, as glasgow-revC3's was. Its spread is between the orders and not between runs:
the four orders gave 106, 87, 84 and 120 with 0.0.20, and 111, 87, 85 and 122, 112, 88, 87 and 122, 112, 88,
85 and 120, 112, 88, 84 and 120 in four measurements of this round. What the round gives it is the fill (a
verification 1.7 s for 3.3 s) and its needless vias out; the time that frees is kept for that step and not
given to the second run, and it has as many connections as it had. Nothing was tried on its first run.
**cynthion's eight** (D116's diagnosis stands). The change asked about, no clearance between the copper of the
two nets of a tie inside the tie's own pads, does not make the sense pad's track legal: inside the force pad's
outline the track is within its own pad already, and where it leaves that outline it is at no distance from
the force pad for its first 0.13 mm, outside the tie's pads. The wider exemption that does, for tracks near
tied pads, is the one D72 made and took back when KiCad's DRC gave `shorting_items` on those tracks; and
KiCad reports the project's own crossings, 4 of the 8 on the original board and 7 on cadlab's re-export
(`tests/corpus/projects.toml`, cynthion's `known`). A router that routed them as the project does would have
7 of 8 reported by the oracle. No rule was changed, no branch was pushed and no oracle run was made for it:
this is read from those records, not measured again.
**Tried and not kept** (guide's `docs/benchmarks.md` has those that are guide's):
- *A negotiation ended when it has stopped gaining* (in guide): once six rounds have passed since the points
  shared last fell by a tenth, where it ends twelve rounds after the fewest, and the best round's routes kept
  for the rounds that the pads cut off their pours begin. tinytapeout-demo's rounds, counted: 3335 points
  shared after the first, 113 after the eleventh, 49 to 230 in the 26 after it, its vias 381 to 425. Ended
  sooner, the passes after it took the time and more (one order's first run 42 s for 35 s). Over four orders
  at 150 million nodes: tinytapeout-demo 4 open for 2, corne-cherry 10 for 5, glasgow-revC3 25 for 24.
- *guide's searches for fewer vias in its last run alone*, by an option of guide's made for it and not
  released. The first run ends sooner (cynthion's at 24.5 s for 30.5 s) and the runs that move routes take
  what it leaves. Where the run for shorter ways is made the board
  ends within two in a hundred of the same; where it is not, the vias of every run stay: cynthion 446 at a
  minute for 423 by the medians of four orders, tinytapeout-demo 416 for 382.5.
- *The escapes planned round the routes in every run from routes* (in guide): above.
- *Half of the run for shorter ways for its searches by their share* (in guide): its cleanup is not ended by
  its share there, tinytapeout-demo's had 11.5 s of 13 s without.
- *A sixth of the budget kept for the last run from the second run that moves routes on*: measured only with
  three other changes, on six boards, with nothing to tell it by, and left out.
**What got worse.** *The pads short of two necks*: 70 to 76 for 64 to 75, above. *tinytapeout-demo's second
order* has 6 open as it had 7, and its fourth 1 for none: 0, 6, 0 and 1 for 0, 7, 0 and 0, the range no wider.
*glasgow-revC3's track*: +30 % for +29 %. *Against the native router*, which has the faster fill: guide is no
longer ahead in every run on glasgow-revD1, and the twelve boards are 102 to 140 against 178 to 268 where they
were 102 to 139 against 335 to 376. No board has more connections open by its range.
**Gaps, for guide's next release**, most useful first:
1. *The first run's time on the boards that take its whole share* (tinytapeout-demo, glasgow-revD1,
   glasgow-revC3): its negotiation and its passes fill 42 s of 60, and everything after them
   shares the rest. Ending the negotiation sooner costs connections (above); what was not tried is a share
   by what the rounds still gain against what the passes then need.
2. *tinytapeout-demo's vias* (+46 %) *and track* (+20 %), made in its negotiation: 203 vias after the first
   round, 391 after the thirteenth.
3. *A run that moves routes cuts some 60 nets as it begins* on a board with an array, by the escapes planned
   again (cynthion 74 to 78 of 334), and routes them again: it is how balls left shut in come out, and it is
   most of what such a run does. A way to those balls that cuts only what is in their way was not looked for.
4. *The array of glasgow-revD1* (14 open with five minutes, all at U8) *and what is left of glasgow-revC3's*
   (4 to 7), *corne-cherry* (0 to 2), *rule areas with a routed track width*, *sweep-v2.2's time*: not touched.
5. *cadlab's fill on boards of many small zones*, and its whole-layer booleans made for the changed places
   alone: corne-cherry and cynthion fill no faster.
6. *A track that leaves a net tie's pad within the tie* (cynthion's eight): above.
**Default: unchanged.** By the ranges of four runs of each, with 0.0.21, guide is behind the native router in
every run on no board: level on 5 (buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight, nrfmicro,
sweep-v2.2); ahead in every run on 4 (buspirate5-rev10 0 against 2 to 12; lumenpnp-mobo 0 against 1 to 24;
cynthion 8 against 28 to 29; glasgow-revC3 4 to 7 against 31 to 36); ranges that overlap on 3 (corne-cherry 0
to 2 against 0 to 4; tinytapeout-demo 0 to 6 against 2 to 14; glasgow-revD1 84 to 120 against 99 to 169; by
their medians 1 against 3, 0.5 against 7.5 and 100 against 111.5). `router: guide` stays the second opinion and
the native router stays the default: the switch is the user's to make.
*Not measured:* freerouting beside 0.0.21; the generated boards; more than four runs of either router; KiCad's
DRC on a board guide routed; cadlab's oracles; cadlab's whole test suite, which is run at the merge; the fill's
times on another machine than this one.

### D119. Thermal relief, third round: what a pad is solid for, relief required at export by a rule, what stands in a spoke's way named, a spoke trimmed down to the minimum track and no further, a repair that takes its fills from the fill before (2026-11-22)
D112 left a repair of 14 s on the largest board, 55 pads short of two spokes on the corpus as its projects
route it (70 to 76 after guide, 131 to 159 after the native router, D117), each with the same sentence for a
reason, no way to say a pad is solid on purpose but a bare `solid`, and thermal findings that never stop an
export.
**1. `zone.relieve` in less time, the same result.** Timed again first, on the code of D117 (release, three
threads, a quiet machine, twice each; `route_bench --corpus` with `CADLAB_BENCH_ORIGINAL=1
CADLAB_BENCH_THERMALS=relieve`, which times `thermal::relieve` on the board as its project routes it):
glasgow-revD1 5.9 s where a fill of that board takes 1.4 s and reading its pads 0.7 s, cynthion 9.6 s (0.8 s
and 0.6 s). (D112's 13.9 s was the command from the command line, with load and save, before D117's faster
fill.) Where the time went, counted:
the repair fills the board and reads every pad after each of its changes, up to eight times (after the zones
and footprints, after the footprints' narrower spokes, after the reliefs at the minimums, after those taken
back, the pours with every pad solid, after the tracks, after the tracks taken out, after the pads kept
solid), and each of those was a fill of every zone layer and a reading of every pad.
- *Zone layers kept* (`zones::LayerMemo`). `fill_zones` keeps the last fill of each zone layer with
  everything it was made from (`LayerKey`, compared in full) and its allowed area. A zone layer whose inputs
  are as they were is taken whole; one where only pads' reliefs changed keeps its allowed area (the offsets
  of every item of another net and their difference: 0.5 s of the 1.8 s that glasgow-revD1's busiest layer
  takes beside its spokes, timed once on a machine with other work). One fill kept per zone layer, ten million
  points in all: with two fills per layer and the allowed area by a key of its own, cynthion's 61 zone layers
  went over the bound at every fill and nothing was ever found again (measured: its refills took what a fill
  from nothing takes).
- *Joints kept* (`thermal::JointMemo`). The joint of a pad on a layer is a function of the pad, its zone and
  relief, the zone copper and the other copper of its net near it, and the footprint's own pads beside it
  (`JointKey`, `pad_joint`); kept by that, compared in full, read side by side with the `parallel` feature.
- *One fill less, one reading less*: the pours with every pad solid are filled without being kept as a
  board's fills (`fill_zones_once`), the joints the command has read are handed to the repair
  (`relieve_from`), and no board is read twice in a row.
- *The same result.* `thermal::relieve_uncached` is the repair with every fill and every reading from scratch.
  `tests/zone_relieve.rs` runs both on every corpus board as routed (release, `CADLAB_CORPUS_DIR`) and asserts
  the same board, the same report and the same joints, and that the joints kept are those of the board read
  again from nothing; `relieve_is_the_relieve_from_scratch` does so on five small boards, twice, the second
  time with everything the first left. `tests/zone_refill.rs` (the fills of `fill_zones` and
  `fill_zones_uncached` on every corpus board in eight states) holds with the zone layers kept. The repairs
  of this code and of D117's are not the same repairs: items 2 below change what a spoke is.

Times of `thermal::relieve`, as above. On a quiet machine, twice each, D117's code beside this decision's
one commit before its last (56377fc: the trimming rule of item 2 in its first form, the same fills and
readings kept); and D117's code beside the code as it stands in one half hour on a machine with other work,
the better of two:

| board | fill + reading (D117) | relieve, D117, quiet | 56377fc, quiet | in fills and readings | D117, busy | as it stands, busy |
|---|---|---|---|---|---|---|
| glasgow-revD1 | 1.43 + 0.67 s | 5.92, 5.95 s | 5.04, 5.08 s | 2.8 to 2.4 | 9.19 s | 4.93 s |
| cynthion | 0.76 + 0.62 s | 9.60, 9.66 s | 5.87, 5.88 s | 7.0 to 4.3 | 12.72 s | 5.99 s |
| lumenpnp-mobo | 0.87 + 0.61 s | 5.67, 5.46 s | 3.39, 3.42 s | 3.8 to 2.3 | 6.06 s | 3.50 s |
| tinytapeout-demo | 0.48 + 0.31 s | 3.28, 3.27 s | 1.80, 1.85 s | 4.2 to 2.3 | 3.74 s | 1.70 s |
| buspirate5-rev10 | 0.43 + 0.25 s | 2.44, 2.39 s | 2.06, 2.10 s | 3.5 to 3.1 | 2.61 s | 1.77 s |

*Not reached:* the repair was to take what one or two fills take, and takes 2.3 to 4.3 fills and readings
of the board (the code as it stands was not timed on a quiet machine). What is left is the
booleans of a whole layer: a zone layer in which one pad's relief changed still has its pour cut, opened by
the minimum width, clipped to its pads' windows, joined with its spokes and searched for islands again, and
on glasgow-revD1 that one layer is most of each of five fills (0.65 to 0.93 s each, of 1.4 s, timed on a
machine with other work). The first fill after an import gains more than the repair: `kicad_import::import`
fills the board once (counted: one `fill_zones`, cynthion's 61 zone layers made), and the fill a command
asks for next finds all 61 again (cynthion 0.24 s for 0.76 s; glasgow-revD1, lumenpnp-mobo, tinytapeout-demo
and buspirate5-rev10 as before, within a tenth).
**2. The pads still short of two spokes: what blocks each, and what was done.** Each was looked at by
direction (`thermal::blockers`, BOARD.md "What is in the way"): for a pad short of spokes, every direction of
its relief in which no copper of its net crosses its outline, the spoke's rectangle against what the fill
keeps away from. Three things came out of the 55 of D112.
- *A trimmed spoke was copper of any width.* D51 kept a trimmed spoke that joined the pour over half the
  spoke's width and cut it to the pad's gap and the pour. So half of a 0.4 mm spoke could not be trimmed
  below 0.2 mm on a board whose tracks go down to 0.09 mm, and with the reliefs at the minimums that D112's
  repair sets, half was 0.045 to 0.075 mm of copper, which no fab makes and no check saw
  (`drc.thermal_size` reads the relief's nominal width); where the pour's rounded edge met the gap, the
  copper kept was thinner still. **A spoke is now trimmed down to the minimum track width and no further**
  (`Relief::min`, `zones::trimmed_spoke`): the spoke within the allowed area, shrunk by half of
  `min_track_width`, the pieces that still reach from the pad to the pour grown again. This is the narrower
  spoke "down to the rule minimum where the full width does not fit", in the fill itself and for every
  board, not only for the pads the repair takes. With no minimum track in the rules D51's test stands.
- *One piece of copper was one spoke.* A zone that sets spokes of 0.4064 mm joins a pad 0.4 mm wide by copper
  that wraps its end: one piece in the band around the outline, 0.74 mm of it, read as one neck. The rules
  now count necks by their copper (`Joint::worth`: the pieces, or the copper on the outline over the spoke
  width when that is more), which is also nearer to KiCad, whose `starved_thermal` counts the spokes it drew.
  The census has both counts below; the repair still works on the pieces (see "Tried and dropped").
- *The reason given was the same for all* ("a relief at the minimums brings no further spoke, no track
  reaches the pour"). `drc.starved_thermal` and the repair now name what stands in each open direction, the
  tracks and vias first and as subjects (`track#12`, `via#7`, six at most), and the hint begins with whose
  it is to move: for tracks and vias of other nets, `route.rip <their nets>` and route again.
What else changed in the repair:
- *A track around what is in the way* (`thermal::detour`). Where no straight track from the pad's middle
  reaches the pour, a way through the copper the net may have is looked for on a grid of 0.05 mm, a spoke's
  width of air kept from the spokes the pad has, ending in the pour beyond the relief gap, pulled straight,
  five segments at most. Two on the corpus as routed (glasgow-revD1 C72.2, cynthion D24.1, 1.0 mm in three
  segments each). For the other pads the search ends within a few hundred cells: there is no way.
- *A pad in a pour that nothing joins and that no pour joined before* (a router's tracks around it) is tried
  solid on that layer and kept so where that joins it, taken back where it does not.
- *Spokes from the pad's ends or at further angles* were not added: on the pads left, the directions that
  are blocked are blocked along the whole side (a track along it, the next pad, the zone's outline), and
  where copper leads round a corner `detour` finds it as a track.

Corpus, twelve boards as routed by their projects (pad and layer pairs; "short" is `drc.starved_thermal` per
pad and layer, by pieces as D112 counted and by copper as the rule now counts; measured by
`tests/zone_relieve.rs`):

| | pads in pours | by necks 0/1/2/3/4+ | short, by pieces | short, by copper | kept solid by the repair | cut off |
|---|---|---|---|---|---|---|
| D112, as routed | 2378 | 2/324/284/239/555 | 116 | | | |
| D112, after its `zone.relieve` | 2380 | 0/207/466/400/760 | 55 | | 43 | 0 |
| now, as routed | 2378 | 1/308/244/256/583 | 95 | 68 | | |
| now, after `zone.relieve` | 2380 | 0/207/436/418/787 | 56 | 32 | 10 | 0 |

After `zone.relieve`, by board (short by pieces, D112 to now; then by copper; then the pads the repair keeps
solid, D112 to now):

| board | short, by pieces | by copper | kept solid | of those short by copper, what is in the way |
|---|---|---|---|---|
| cynthion | 23 to 31 | 17 | 31 to 1 | J1, J2 pins A4, A9, B4, B9: the next pins of the connector and the zone's outline, for three also tracks; IC1.G2, IC1.H3: the balls around them; C79.2, R77.2, R94.1, R95.1, U10.E5: tracks and vias; Q5.4, R73.2: another net's pour, a via |
| buspirate5-rev10 | 10 to 10 | 7 | 0 | U304 to U308 pin 2: a track and a via at the free end; J202.A4B9, J202.B4A9: the next pins, a non-plated hole, the zone's outline |
| glasgow-revD1 | 7 to 6 | 1 | 0 to 1 | U34.2: two tracks, the pads of D48 |
| glasgow-revC3 | 4 to 4 | 2 | 12 to 8 | U14.1, U31.1: two vias, another net's pour, the zone's outline |
| lumenpnp-mobo | 8 to 2 | 2 | 0 | C77.2, C91.2: the pads of two resistors, a via, another net's pour |
| tinytapeout-demo | 2 to 2 | 2 | 0 | C34.2, C42.2: tracks |
| sweep-v2.2 | 1 to 1 | 1 | 0 | U1.4: a track, the board edge |
| the other five | 0 | 0 | 0 | |

Read as it is: by D112's count the repair leaves as many pads on one piece of copper as before (55, 56);
what moved is which. 33 of the 43 pads that D112 kept solid for want of any joint are relieved now, the 26
balls of cynthion's `IC1` among them, most of them on one spoke trimmed to the 0.1 mm of that board's
minimum track, which is why cynthion's count rises; lumenpnp-mobo's motor drivers' pin 25 and four pads of
glasgow-revD1 have their second spoke. By the copper, 32 pads are short of two spokes, each with what is in
its way named: tracks and vias of other nets on about half of them (the router's), the next pins of the
pad's own connector or array and the outline of a small zone on 10, pads of other parts and other pours on
the rest. No pad is left without a neck. The repair gave 191 pads a relief of their own (225), added 13
tracks (9), kept 10 pads solid (43).
After the routers (60 s, three threads, one run each on a machine with other work, the routers of D117;
"before" is the same day's run of D117's code, which is another routing, the routers ending by time; all
counts by `route_bench --corpus` with `CADLAB_BENCH_THERMALS=relieve`, "short" as each code counts it):

| | open connections | pads in pours | no neck | on one neck | short | after `zone.relieve`: no neck | on one neck | short | spokes that cover the pad |
|---|---|---|---|---|---|---|---|---|---|
| guide, D117's code | 250 | 2370 | 2 | 257 | 63 (pieces) | 0 | 166 | 21 (pieces) | 3 |
| guide, now | 293 | 2374 | 0 | 228 | 41 (copper) | 0 | 142 | 23 (copper) | 2 |
| native router, D117's code | 309 | 2353 | 43 | 275 | 154 (pieces) | 2 | 164 | 37 (pieces) | 4 |
| native router, now | 308 | 2359 | 25 | 243 | 90 (copper) | 0 | 136 | 22 (copper) | 4 |

No pad is without a neck after the repair on either router's boards (two were, on glasgow-revD1 after the
native router with D117's code; and one in an earlier run of this code, before the pads that nothing joined
were tried solid: U34.7 of that board, with tracks of two nets and a via in its way. Whether that step is
what leaves none now cannot be told from another routing). The pads on one neck are fewer after the repair
with either router (166 to 142, 164 to 136: counted the same way before and now, on different routings).
The counts of "short" are not comparable across the two codes (pieces, copper), and one run a side says
nothing about the routers against each other: guide's run "now" left 293 connections open for 250, on a
busier machine (glasgow-revD1 247 and 219).

**3. What a pad is solid for** (`PadPurpose`: `press_fit`, `terminal`, `heat`, `stitching`; BOARD.md has the
table). Stored on the placed pad (`PadJoin::purpose`), the pad or the footprint (`Overrides::purpose`), an
enum, absent when unset, so boards and libraries without it are written as before. `zone.pad --purpose` and
`footprint.set --purpose` set it and, unless a connection is given or set, `solid` with it. A press-fit pin and
a stitching pad are not soldered (`PadClass::Bare`), a heat pad is a heat pad, a terminal is the solder pad
it is and solid by its purpose; `drc.solid_pad` is quiet for all four, `zone.thermals` lists them (`meant`,
the purpose), and `zone.relieve` keeps their setting where it removes a `solid` that says nothing. The default
did not move: a through-hole pin or an SMD pad in a pour is relieved.
*KiCad.* The published file format ("S-Expression Format", footprint pad, read 2026-11-22) has a `property`
token on pads with six values: BGA, global and local fiducial, test point, heat sink, castellated. One says
what a pad is in a pour: `pad_prop_heatsink` is read as `purpose: heat` and written back. It has no value for
a press-fit pin (D112 had left that to be looked up), a terminal or stitching: those are written as the
pad's `zone_connect` and listed by `export.pad_purpose`, and come back as solid pads without a purpose. No
KiCad source was read. The generated exposed pads and tabs keep their `zone_connection: solid` and get no
`purpose`: they are classed as heat pads by their paste windows and size already, and a property on them
would move every golden file of a generated footprint for nothing.
**4. Relief required.** `board.rules --thermal-required true` (`Rules::thermal_required`, stored with the
board, false by default, in no preset and no fab profile): the thermal findings of the fab check
(`fab.solid_pad`, `fab.thermal_nominal`, `fab.starved_thermal`, `fab.thermal_size`) are errors, so
`fab.export` refuses, and no waiver takes an error away. A pad is let through by what item 3 says: a purpose,
or a placed pad's own setting with its reason, which is also what the repair writes for the pads it keeps
solid. A bare `solid` on a pad or footprint is then not taken as meant. `drc.run`'s own findings stay
warnings. *The default stays as it was*, warnings: the user was asked which they want and has not said, and
an error by default refuses the export of every imported board (664 solder pads solid on the corpus as its
projects make it, D109).
**5. Copper on the two pads of a chip part, against each other: not written.** A warning for a two-pad part
with very unequal copper on its pads needs a number for "very". Read for one, 2026-11-22: Eurocircuits, "PCB
assembly guidelines: tombstoning" ("the difference in thermal mass between pads of the same component", "the
aim is to make the track width to each pad of the component the same width", "at least 0.25 mm of track before
it connects to the copper area"); Nordson EFD, "Tombstone troubleshooting"; the DFM pages of JLCPCB, Cadence,
Autodesk and Altium on tombstoning. All say the two pads should have the same copper; none gives a ratio or a
percentage, and IPC-7351's text was not found in what could be read. So no threshold was invented and no rule
added. What there is: `drc.solid_pad` names the risk when one pad of a two-pad part is solid and the other is
not (D109), and Eurocircuits' 0.25 mm of track before a copper area is a number a later rule could stand on.
**Golden files and tests.** One golden file moved and was blessed again on purpose,
`tests/golden/exchange/tiny-ipc2581.xml`: the round pad of the tiny board whose spoke is trimmed has it with
the corners of the trim rounded (four points for two in the plane's outline), and two cutouts in that plane
that were a few nanometers wide (3.5429 to 3.6045 mm by 0.000002 mm: the seam between a trimmed spoke and
the pour's rounded edge) are gone. No other golden file and no snapshot moved: the tests of zones, thermal
relief, DRC, fab check, KiCad import and export, the exchange formats, the model's round trip, the CLI and
MCP snapshots, the Gerber cross-check, the board render and the demo board were run. New tests:
`tests/zone_relieve.rs` (above); in `tests/thermal_relief.rs` the repair against the repair from scratch,
`solid_for_a_purpose`, `thermal_relief_required`, `relieve_joins_a_pad_nothing_joined`, and the blockers in
`starved_thermal`; in `board::thermal` `purposes`, `a_track_around_the_corner` and the copper count in
`solid_and_starved`; in `board::zones` `trimmed_spoke_is_no_narrower_than_a_track` and the allowed area kept
in `reliefs_reused_give_the_fill_made_from_scratch`.
**KiCad.** In CI, on the branch `ci/thermal-3`, KiCad 10 (the oracle and corpus jobs; `gh workflow run CI`):
- commit 56377fc (the first form of the trimming rule): `Oracle tests` and `Open-source corpus` passed, as
  did `Test & lint` on Linux and macOS (the whole suite); the run was cancelled there.
- commit cd7e4bc (the rule as it stands): `Oracle tests` failed in the Gerber cross-check alone: on the
  features board the fourth spoke of J1.1, the pin at the board edge, is now the one on the diagonal beside
  the edge, trimmed to what the edge clearance leaves (0.063 mm² on `B.Cu` at x 9.69 to 10.07, y 1.06 to
  1.37 mm), where D112's was the whole one across the pad: of the directions that fit, the one that spreads
  best is taken, and more fit now. KiCad drops that spoke as it dropped D112's. The entry of `KNOWN` was
  moved and says so. `kicad_agrees_on_starved_thermals` passed: `starved3` reported by both, `starved2` by
  neither, and on `turned3` no finding on the exported fill (KiCad counts the diagonals cadlab places) and
  "zone min spoke count 2; actual 1" on KiCad's own refill, as in D112.
- commit 7d28cf8 (the same code, `KNOWN` moved): `Oracle tests` and `Open-source corpus` passed (KiCad
  10.0.8): all twelve projects `ok`, no finding without an allowance on either side. On the corpus the two
  tools still count on their own fills (the allowances of D109, on both sides): cadlab reports 68 pads as
  routed on five boards (39, 14, 11, 3 and 1), KiCad its own on its refills. `Test & lint` (the whole
  suite, fmt, clippy, rustdoc) passed on Linux, macOS and Windows, as did the freerouting, exchange and
  openEMS oracles, MSRV 1.95, cargo-deny and the benchmarks. The run was cancelled with the router on the
  corpus at a fixed work budget (informative), ems beside openEMS and freerouting on the corpus still
  going; the branch is deleted.
**Tried and dropped.**
1. *The repair read place by place*: each change measured on a fill of the places around it alone
   (`fill_zones_within`: the zones cut to windows around the changed pads and the pads beside them), the whole
   board filled once at the end and compared, the slow way taken when the two disagree. Built and measured on
   glasgow-revD1: the footprints' narrower spokes change 241 placed pads and the reliefs at the minimums 99,
   and with the margins a change can reach (a relief, twice the minimum width, a clearance) their windows
   took in 2541 and 2473 of the board's 2557 pads: a fill of the places was a fill of the board. And a fill
   cut to a window is the same region and not the same polygons, so the result was the reference's only as
   long as no decision sat on an arc's last micrometer. Taken out.
2. *Two fills kept per zone layer and the allowed area by a key of its own*: over the bound on cynthion at
   every fill (above).
3. *The repair working on necks by their copper*: a pad on one wide piece was then not tried with a narrower
   relief, and three pads of cynthion ended without effective relief. The repair works on the pieces as
   before; the rule counts by copper.
4. *A trimmed spoke dropped when narrower than a track, and D51's half-width test kept beside it*: the first
   form of the rule. On the corpus as routed it only took spokes away (nine more pads on one neck after the
   repair, 63 short by pieces for 55), and the U of D112's own test, with legs as long as it is wide, went
   back to one spoke: what D51 cut to the gap and the pour was pinched where the pour's rounded edge meets
   the gap, however wide the way. The rule as it stands (trimmed to the minimum track, within the allowed
   area) keeps that U's diagonals and 95 pads short as routed for 116.
**What got worse.** cynthion's pads short of two spokes (above: 23 to 31 by pieces, pads that were solid
before). Fills are not those of D117 where a spoke is trimmed: fewer where what was left was thinner than a
track, more where it was thinner than half the spoke and still a track, and a trimmed spoke is no longer cut
to the pad's gap and the pour, so it can run through allowed copper that the pour's minimum width removed
(it is a track's width of copper there, and keeps every clearance: it lies in the allowed area). Of the
other directions that fit, the one that spreads best is still the one taken, and a trimmed one can now be
taken where a whole one stood before (J1.1 of the features board, under **KiCad**): thinner copper, better
spread; a whole spoke was not given the preference. A pad whose
spokes at the minimum track width still cover two thirds of its outline is left as `drc.thermal_nominal`
after the repair on boards the routers made: the repair narrows spokes and does not take one away.
`LayerMemo` and `JointMemo` hold up to ten and two million points more in a process that fills (160 and 32 MB
at the bound). A fill of a board from nothing takes the building of the keys more (not measured apart). The
diagnostic's "its footprint leaves room for N" (`Joint::possible`, the zone's clearance around the boxes of
the footprint's pads) and what `blockers` finds in the way (the clearances the fill keeps, round the pads'
shapes) are two tests: on two balls of cynthion's `IC1` the first says two directions are free and the second
finds the footprint's own pads in all of them.
*Not done:* a router that moves a track off a pad's spoke on being told (`thermal::blockers` names them;
BOARD.md, "What a router must be told", says what the guide adapter would pass); fewer spokes for a pad that
four of them cover; the refill of a changed zone layer in less than its whole-layer booleans; `purpose` on
generated footprints; the 3D view.
