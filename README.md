# cadlab

**Headless electronics CAD, written in Rust, designed to be driven by programs and AI agents.**

cadlab covers the whole path from an idea to a manufacturable PCB, without a GUI:

```
parts & BOM  →  circuit (netlist)  →  board layout  →  autorouting  →  DRC  →  fab outputs
   research        ERC, schematic         stackup,        native Rust       checks     Gerber, drill,
   sourcing        rendering              placement       router                       P&P, BOM
```

The goal is to cover what KiCad and freerouting do today, minus the interactive UI. The only visual output
is rendering: PNG/SVG of schematics and boards, and later isometric 3D previews.

## Three ways in, one engine

| Surface | For | How |
|---|---|---|
| **Rust library** (`cadlab` crate) | Embedding in other tools, scripting, tests | `use cadlab::prelude::*;` |
| **CLI** (`cadlab` binary) | Humans, shell scripts, CI | `cadlab part add --mpn RC0402FR-0710KL` |
| **MCP server** (`cadlab mcp`) | AI agents (Claude, etc.) | stdio transport (streamable HTTP later) |

All three are thin layers over the same **command registry**. Every mutation and query is a typed, serializable
command with a JSON Schema, so anything you can do from Rust you can also do from the shell or from an agent,
with the same validation and the same error messages. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Status

Pre-alpha. **M0 (foundations) is done**: project files, the command system with transactions and undo, the CLI
and the MCP server. Only `project.*` and `history.*` commands exist so far; parts and BOM (M1) come next. See the
[roadmap](docs/ROADMAP.md).

```sh
cargo build --release            # binary: target/release/cadlab

cadlab project new myboard --targets jlcpcb,pcbway --description "LED blinker"
cd myboard
cadlab project set --metadata rev=A
cadlab project info --json
cadlab undo
cadlab describe                  # every command; `cadlab describe project.set` for one
cadlab mcp                       # MCP server on stdio
```

MCP client configuration (e.g. Claude Code): `claude mcp add cadlab -- /path/to/cadlab mcp`.

## Documentation

| Document | Contents |
|---|---|
| [docs/ROADMAP.md](docs/ROADMAP.md) | Milestones, scope, exit criteria |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate layout, command system, core principles |
| [docs/DATA_MODEL.md](docs/DATA_MODEL.md) | Project model, units, IDs, on-disk format |
| [docs/INTERFACES.md](docs/INTERFACES.md) | Library API, CLI conventions, MCP server design |
| [docs/PARTS.md](docs/PARTS.md) | Parts, libraries, BOM, supplier research, footprint generation |
| [docs/ROUTER.md](docs/ROUTER.md) | Autorouter design (freerouting replacement) |
| [docs/RENDERING.md](docs/RENDERING.md) | PNG/SVG rendering, isometric 3D |
| [docs/MANUFACTURING.md](docs/MANUFACTURING.md) | Industry file formats, fab profiles (JLCPCB, PCBWay, ...) |
| [docs/TESTING.md](docs/TESTING.md) | Test layers, verification oracles (KiCad, freerouting, ...) |
| [docs/POLYGON_LIB.md](docs/POLYGON_LIB.md) | Requirements for the standalone polygon geometry crate |
| [docs/DECISIONS.md](docs/DECISIONS.md) | Decision log and open questions |

## Design principles (short version)

1. **API first.** The library is the product. The CLI and MCP are generated from it, not written alongside it.
2. **Netlist is the source of truth.** Connectivity is defined directly. Schematic drawings are a derived view,
   generated automatically for human review.
3. **Agent-friendly by construction.** Stable human-readable identifiers (`R12`, `net:VBUS`), deterministic output,
   structured errors with fix hints, dry-run and batch transactions, compact text summaries.
4. **Exact geometry.** Integer nanometer coordinates. No floating-point drift in stored data.
5. **Standards-based.** Own native save format; industry standards (Gerber X2/X3, Excellon, IPC-D-356A, IPC-2581,
   IPC-7351, Specctra DSN/SES, STEP) for everything exchanged. KiCad and freerouting are used only as external
   verification oracles, never as code sources.
6. **Deterministic.** Same input, same output, byte for byte, including the router (seeded).

## License

MIT. No code from GPL projects (KiCad, freerouting, ...) may enter this repository. See
[docs/DECISIONS.md](docs/DECISIONS.md) (D7).
