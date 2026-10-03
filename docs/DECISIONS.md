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

## Open questions

None currently.
