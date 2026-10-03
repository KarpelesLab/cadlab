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

## Open questions

None currently.
