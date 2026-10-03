# Architecture

## Crate layout

**One crate**, `cadlab`: the library plus the `cadlab` binary (CLI + MCP server, behind the default `cli` feature
so library users can drop the CLI dependencies with `default-features = false`). Subsystems are modules, not
crates. When a piece is useful on its own, it is spun out as a **standalone, general-purpose crate** in its own
repository, the way polyclip was (DECISIONS D16).

```
cadlab/
├── Cargo.toml
├── src/
│   ├── lib.rs                 # crate root: re-exports, registry(), prelude
│   ├── units.rs geom.rs id.rs refs.rs diag.rs suggest.rs   # core types; geom wraps polyclip
│   ├── model/                 # project data model, on-disk format, migrations
│   ├── command/               # command system: trait, registry, sessions, transactions, undo history
│   ├── commands/              # built-in commands, one file per group (project, history, ...)
│   └── bin/cadlab/            # the binary: main.rs, cli.rs (schema → clap), mcp.rs (MCP server)
├── tests/                     # integration tests; common/ (golden files, oracles), golden/, snapshots/
├── fab-profiles/              # JLCPCB, PCBWay, ... (TOML, with sources and verification dates), from M4
└── docs/
```

Modules to come, by milestone: `parts` and `suppliers` (M1), `erc` (M2), `schematic` and `render` (M3), `board`,
`drc`, `io` and `fab` (M4), `router` (M5). Spin-off candidates later: the router, Gerber/Excellon I/O, the
footprint generator.

Dependency direction between modules is strictly downward, and enforced by review:

```
        bin/cadlab (CLI, MCP)
                 │
       commands  →  command
                 │
   parts  erc  board  router  render  io  fab  suppliers
                 │
               model
                 │
   units  geom  id  refs  diag  suggest
```

Algorithm modules (router, DRC, renderer, exporters) take the model by reference and return results. They do not
use `command`/`commands` or the binary. That keeps them testable and easy to spin out.

## The command system

This is the central design decision. Every operation that a user, a script or an agent can perform is a
**command**: a plain struct that is `Serialize + Deserialize + JsonSchema`, with a typed output.

```rust
// src/command/command.rs
pub trait Command: Serialize + DeserializeOwned + JsonSchema {
    const NAME: &'static str;                       // "project.set", "bom.add", "route.net"
    const SUMMARY: &'static str;                    // one line, for help and tool listings
    const KIND: CommandKind;                        // Query | Mutation | Session
    const POSITIONAL: &'static [&'static str] = &[]; // CLI positional arguments

    type Output: Serialize + JsonSchema;

    fn run(self, ctx: &mut Context<'_>) -> Result<Self::Output, CommandError>;
    fn summarize(output: &Self::Output) -> String;  // short text for CLI/MCP
}
```

- **Query** reads only. **Mutation** changes the project inside a transaction. **Session** manages the session
  itself (`project.new/open/save`, `history.undo/redo`): not transactional, not allowed in batches.
- Long-running behavior (progress, cancellation) is available to any command through the `Context`.
- Field doc comments become schema descriptions, which become CLI help and MCP tool docs. Write them for users.

The registry is an explicit list (`commands::register_all`), so ordering is deterministic. It provides:

- **Rust API:** `cadlab::command::run(&mut session, project::Set { .. }, opts)` (typed), or
  `registry.execute(&mut session, "project.set", json_args, opts)` (by name). Ergonomic wrappers
  (`project.bom().add(..)`) can be layered on top later.
- **CLI:** each command maps to a subcommand (`bom.add` → `cadlab bom add`). Flags are derived from the schema
  (see `src/bin/cadlab/cli.rs` for the mapping rules).
- **MCP:** each command (or group, see [INTERFACES.md](INTERFACES.md)) becomes a tool whose input schema is the
  command's JSON Schema.
- **Batch/replay:** a JSONL file of `{"cmd": "...", "args": {...}}` lines is a valid script. The project's
  operation log uses the same format, so any session can be replayed.

### Transactions, undo, dry-run

- Mutations run against a **snapshot**: project sections are `Arc`s with copy-on-write, so a snapshot is a cheap
  clone. On error or dry-run the snapshot is restored; on success it becomes the undo entry. If profiling ever
  shows whole-section copies are too costly (huge boards), sections can be split further or moved to persistent
  collections without changing the command API.
- `--dry-run` / `"dry_run": true` runs the command, collects the result and diagnostics, then rolls back. Agents
  use this to preview the effect of an operation.
- A batch is one transaction by default: all-or-nothing.
- A command that leaves the project equal to its snapshot records no undo step.
- Undo/redo stacks live in the `Session`, persisted under `.cadlab/history/` (index + one packed snapshot per
  step, loaded lazily, 100 steps max), so undo works across CLI invocations. Executed steps are appended to
  `.cadlab/oplog.jsonl` in batch format.

### Diagnostics and errors

Every command returns, besides its output, a list of `Diagnostic`s:

```rust
pub struct Diagnostic {
    pub severity: Severity,         // Error | Warning | Info
    pub code: Cow<'static, str>,    // stable, e.g. "drc.clearance", "bom.part_not_found"
    pub message: String,            // human-readable
    pub subjects: Vec<ObjectRef>,   // what it concerns: R12, net:VBUS, pad U1.4, ...
    pub location: Option<Point>,    // board/schematic position if relevant
    pub hint: Option<String>,       // how to fix it, written for agents
}
```

Errors (`CommandError`) are a `Diagnostic` plus an `ErrorKind` (`invalid_args`, `not_found`, `conflict`,
`no_project`, `io`, `cancelled`, `internal`). Unknown identifiers come with "did you mean" suggestions. This matters more than
usual: an agent fixing its own mistakes is only as good as the error messages it gets.

### Long-running commands

Routing, zone fill on large boards, and supplier searches can take time. They:

- report progress through a `Progress` sink (CLI progress bar, MCP progress notifications),
- support cancellation (cooperative, checked in inner loops),
- accept a time budget, and return the best partial result when it runs out.

## Core principles in code

- **Integer geometry.** Coordinates are `i64` nanometers. Floating point is allowed only inside algorithms
  (e.g. arc approximation), never in stored data. See [DATA_MODEL.md](DATA_MODEL.md).
- **No global state.** Everything goes through a `Project` value. Several projects can be open in one process,
  which the MCP server needs.
- **Determinism.** Stable iteration order (`IndexMap`/`BTreeMap`, never `HashMap` iteration in output paths),
  seeded RNG in the router, stable sort keys.
- **No LLM inside the library.** cadlab is a tool that agents use. It does not call models itself. Anything
  requiring judgement (picking a part, reading a datasheet) is exposed as data and commands, and the agent decides.
- **No async runtime in core.** Core is sync and CPU-bound (rayon for parallelism). The MCP server is sync too
  (std threads). Only network I/O for suppliers may use an async runtime, confined to the `suppliers` module behind a
  blocking API (D14).

## Candidate dependencies

To confirm during M0. Listed so choices are deliberate, not accidental.

| Need | Candidates |
|---|---|
| Serialization | `serde`, `serde_json`, `toml` |
| Schemas | `schemars` |
| CLI | `clap` (derive) |
| MCP | own minimal implementation, sync, std only (decided, D14) |
| Errors | `thiserror` (libs), `anyhow` (binary only) |
| Polygon booleans / offsetting | standalone MIT crate built to [POLYGON_LIB.md](POLYGON_LIB.md) (decided, D9) |
| Spatial index | `rstar` |
| Graphs | `petgraph` |
| Parallelism | `rayon` |
| Rendering | own SVG writer, `resvg` + `tiny-skia` for PNG, `ab_glyph`/`fontdue` for text |
| HTTP | `reqwest` (rustls) |
| S-expressions (KiCad) | small hand-written parser, written from the published format docs |
| License policy | `cargo-deny`: MIT-compatible dependencies only, no GPL/LGPL/AGPL |
| Testing | `insta` (snapshots), `proptest` (geometry) |
