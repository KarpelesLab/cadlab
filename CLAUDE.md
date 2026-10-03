# cadlab — notes for agents working on this repo

Headless Rust EDA: parts/BOM → circuit → board → autorouter → DRC → fab outputs. Library first; the `cadlab` binary
exposes the same operations as a CLI and an MCP server. Read `docs/ARCHITECTURE.md` before adding code.

Rules:
- Every user-facing operation is a `Command` in the registry (`src/command`, commands in `src/commands`). Never add an operation only to the
  CLI or only to MCP.
- Coordinates are `Nm` (i64 nanometers). No `f64` in stored model data. Length inputs always carry units.
- Output must be deterministic: no `HashMap` iteration in output paths, seeded RNG only.
- Single crate (DECISIONS D16). Algorithm modules (router, drc, render, io) must not use `command`/`commands` or
  the binary. Generally useful pieces get spun out as standalone crates in their own repo (like polyclip).
- Errors/diagnostics need a stable `code`, the subjects involved, and a fix `hint`.
- No LLM calls inside the library.
- License is MIT. Never read-and-port, copy or translate code from KiCad, freerouting or any GPL project, and never
  bundle or convert KiCad libraries. Those tools are external test oracles only (docs/TESTING.md). File formats are
  implemented from published specs/docs.
- Native save format is our own; anything exported uses industry standards (docs/MANUFACTURING.md).
- Polygon geometry comes from the standalone crate specified in docs/POLYGON_LIB.md, accessed only through
  `cadlab::geom`.
- Fab-specific values live in fab profile data files with source + verification date, never hard-coded.
- Projects are provider-agnostic: nothing fab- or supplier-specific is stored in the design. Fab choices (SKUs,
  rotation offsets, file layouts) are applied at export and recorded in `fab-lock.json` (DECISIONS D12).
- Update `docs/ROADMAP.md` checkboxes and `docs/DECISIONS.md` when finishing items or making design choices.

Development:
- `cargo test` (all), `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all`. CI also runs docs with
  `-D warnings`, MSRV 1.89 and `cargo-deny`.
- Golden files: `CADLAB_BLESS=1 cargo test` rewrites them; snapshots: `cargo insta review`.
- Adding a command: a struct implementing `Command` in `src/commands/<group>.rs`, registered
  in that file's `register`. CLI flags and MCP tools are generated from it; doc comments become help text.
