# Interfaces: library, CLI, MCP

All three are views of the [command registry](ARCHITECTURE.md#the-command-system). This document fixes the
conventions for each.

## Rust library

The example below is the target API. As of M0, commands are called through `cadlab::commands::run` (typed) or
`Registry::execute` (by name); see the `cadlab` crate docs.

```rust
use cadlab::prelude::*;

let mut p = Project::create("myboard")?;

p.bom().add(PartQuery::generic("R 10k 1% 0402"))?;
let ldo = p.parts().search(Search::new("LDO 3.3V 500mA SOT-23-5").in_stock())?.best()?;
let u2 = p.circuit().add_component(&ldo)?;            // -> "U2"

p.circuit().connect([u2.pin("VIN"), "J1.VBUS".into()], "VBUS")?;
p.circuit().connect([u2.pin("GND")], "GND")?;

let report = p.erc()?;
assert!(report.errors().is_empty());

p.board().outline_rect(mm(50), mm(30))?;
p.board().place("U2", mm(10), mm(15), Angle::ZERO, Side::Top)?;
p.route(RouteOptions::default().time_budget(Duration::from_secs(60)))?;

p.render(RenderTarget::Board { layers: LayerSet::copper_top() }).write_png("top.png")?;
let check = p.fab().check("jlcpcb")?;                 // capabilities + parts availability
p.fab().export("jlcpcb", "out/jlcpcb")?;               // files + fab-lock.json
p.save()?;
```

- Ergonomic wrappers (`p.bom().add(..)`) build and run commands. Power users can run commands directly:
  `p.run(bom::Add { .. })`.
- Fallible operations return `Result<T, CommandError>`. Warnings are available on the result:
  `p.last_diagnostics()`.
- Algorithms are also usable standalone: `cadlab::router::route(&board, &rules, opts)`, without a `Project`.

## CLI

### Shape

```
cadlab [GLOBAL OPTS] <group> <action> [ARGS]

GLOBAL OPTS
  -p, --project <DIR>   project directory (default: walk up from cwd looking for cadlab.toml)
      --json            machine-readable output (JSON object on stdout, diagnostics included)
      --dry-run         run, report, roll back
  -q / -v               verbosity
```

Groups follow the registry namespaces: `project`, `part`, `lib`, `bom`, `circuit`, `net`, `erc`, `schematic`,
`board`, `place`, `track`, `zone`, `drc`, `route`, `render`, `fab`, `export`, `import`.

Examples:

```sh
cadlab project new myboard --layers 2 --targets jlcpcb,pcbway
cadlab part search "LDO 3.3V 500mA SOT-23-5" --in-stock --limit 5
cadlab bom add mpn:AP2112K-3.3TRG1
cadlab circuit add AP2112K-3.3TRG1            # -> U2
cadlab net connect VBUS J1.VBUS U2.VIN C1.1
cadlab erc
cadlab board outline rect 50mm 30mm
cadlab place U2 10mm 15mm --rot 90 --side top
cadlab route --all --budget 60s
cadlab drc
cadlab render board --layers F.Cu,F.SilkS -o top.png
cadlab fab compare --fab jlcpcb,pcbway
cadlab export fab --fab pcbway -o out/pcbway/
```

### Generic entry points

```sh
cadlab call bom.add '{"part": "mpn:AP2112K-3.3TRG1"}'   # any command, JSON args
cadlab batch script.jsonl                               # transaction of many commands
cadlab describe [<command>]                             # list commands / print a command's schema
cadlab undo | cadlab redo | cadlab history
cadlab config digikey | show | path | remove        # user settings: supplier credentials (CLI only, D17)
```

### Conventions

- Human output on stdout, logs and progress on stderr. With `--json`, stdout carries exactly one JSON document.
- Exit codes: `0` ok, `1` command error, `2` usage error, `3` completed but checks failed (ERC/DRC errors).
  CI can then run `cadlab drc` and rely on the exit status.
- Every length argument requires a unit.

## MCP server

`cadlab mcp` starts an MCP server (stdio by default, `--http <addr>` for streamable HTTP).

### Tool design

Exposing hundreds of fine-grained commands as separate tools would flood an agent's context. Strategy:

1. **Domain tools** (~15–25): one tool per group with an `action` field, e.g. `bom` with actions
   `add | remove | replace | list | set_dnp | ...`. Input schema = tagged union of the group's command schemas.
2. **`describe`**: returns the full schema and examples for one command or group, on demand.
3. **`call` / `batch`**: escape hatches identical to the CLI ones.
4. **Sessions**: `project` actions `new` / `open` / `save`. The server keeps projects open in memory between
   calls; later calls act on the most recently opened/created project, or the one named by the `project`
   argument. Changes are autosaved by default; `cadlab mcp --no-autosave` keeps them in memory until
   `project.save`.

Tool groups are the same strings as CLI groups, so docs and examples carry over.

### Output for agents

- Results are JSON plus a short text summary. Large results are paginated or summarized, with a cursor or a
  narrower follow-up command suggested.
- `render` returns MCP **image content** (PNG) so multimodal agents can look at the board or schematic directly,
  optionally with highlighting (`highlight: ["net:VBUS", "U2"]`) and DRC markers.
- Diagnostics always include `code`, `subjects` and `hint` (see ARCHITECTURE.md).
- Long-running tools (route, part search) send progress notifications and honor cancellation.

### Resources

Read-only views for agents that prefer resources over tools:

- `cadlab://{project}/summary` — compact text overview (parts, nets, board status, open issues)
- `cadlab://{project}/bom` — BOM as CSV/JSON
- `cadlab://{project}/circuit` — netlist in a compact text form
- `cadlab://{project}/render/{view}.png` — rendered images

### Prompts

Optional MCP prompts packaging common workflows: "design a board from a requirements list", "source this BOM",
"fix DRC errors".
