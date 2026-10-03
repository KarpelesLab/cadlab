# Standards and manufacturing

## File formats

cadlab's **native save format is its own** (deterministic JSON, see [DATA_MODEL.md](DATA_MODEL.md)). Everything
that leaves cadlab for another tool or a manufacturer uses **industry standard formats**, implemented from their
published specifications.

| Purpose | Standard | Direction | Milestone |
|---|---|---|---|
| Copper, mask, silk, paste, outline | **Gerber X2** (Ucamco spec), RS-274X compatible | export | M4, done (`export.gerber`) |
| Component data (assembly) | **Gerber X3** | export | M4, done (`export.gerber`) |
| Drill / route | **Excellon** (XNC profile, Ucamco), optionally Gerber X2 drill files | export | M4, drills done (`export.drill`); routed slots later |
| Bare-board electrical test netlist | **IPC-D-356A** | export | M4, done (`export.ipc356`) |
| Pick and place | CSV (generic + per-fab column layouts), Gerber X3 | export | M4, done (`export.pnp`; per-fab layouts in `fab.export`) |
| Assembly BOM | CSV / XLSX (generic + per-fab layouts) | export | M1/M4, CSV done (`bom.export`, `fab.export`) |
| Intelligent fab data | **IPC-2581** (rev C) | export | M9 |
| Intelligent fab data | ODB++ (check spec license terms first) | export | later |
| Routing exchange | **Specctra DSN / SES** | import + export | M5 |
| Mechanical CAD exchange | **IDF 3.0**, **IDX** (ProSTEP EDMD) | export | later |
| 3D | **STEP** AP214/AP242 (export), STEP/VRML models (import) | both | M9 |
| Simulation | SPICE netlist (ngspice dialect) | export | M8 |
| Documentation | SVG, PNG, PDF | export | M3/M4 |
| KiCad | `.kicad_pcb`, `.kicad_sch`, `.kicad_sym`, `.kicad_mod`, netlist | import + export | M2–M7, see below |

### Generic fab outputs (`src/fabout/`, `export.*`)

Implemented from the Ucamco Gerber Layer Format Specification (rev. 2026.05) and XNC Format Specification
(rev. 2021.11); IPC-D-356A from its published fixed-column record layout. Output is deterministic (no dates).
`export.all` writes (generic names, defined in one table, `fabout::file_name`):

| File | Content |
|---|---|
| `<project>-F_Cu.gbr`, `-In1_Cu.gbr`, ..., `-B_Cu.gbr` | `Copper,Ln,Top/Inr/Bot`: pads flashed (`C`/`R`/`O`, fixed macros for rounded or rotated pads), tracks drawn (arcs with `G75`), pours as fractured regions; `.N`/`.P`/`.C` object attributes, `.AperFunction` on every aperture |
| `-F_Mask.gbr`, `-B_Mask.gbr` | `Soldermask`, negative (the image is the openings); openings grown by `mask_expansion` (default 0); vias tented |
| `-F_Paste.gbr`, `-B_Paste.gbr` | SMD pads, or the pad's paste windows (exposed pads) |
| `-F_SilkS.gbr`, `-B_SilkS.gbr` | `Legend`: footprint silk, designators (Hershey strokes, mirrored on the bottom), board graphics; clipped at mask openings |
| `-Edge_Cuts.gbr` | `Profile,NP`: outline contours with arcs |
| `-F_Component.gbr`, `-B_Component.gbr` | Gerber X3: `ComponentMain` flash with `.CRot/.CMfr/.CMPN/.CVal/.CMnt/.CFtp/.CPgN/.CHgt`, courtyard outline, pins; DNP excluded |
| `-PTH.drl`, `-NPTH.drl` (`-PTH-L1-L2.drl` for blind/buried spans) | XNC: metric, decimal coordinates, one tool per (function, diameter), X2 attributes in `; #@!` comments. `export.drill {gerber: true}` also writes `-PTH-drl.gbr` X2 drill files |
| `-pos.csv` | Designator, value, package, footprint, X/Y in mm from the outline's lower-left corner, rotation (placement, CCW from the IPC-7351 zero orientation), side; DNP excluded |
| `.d356` | IPC-D-356A, `UNITS CUST 1`: `327` SMD pads, `317` plated holes and vias (mid-net `M`, tented `S3`), `367` non-plated holes |

Gerber, drill and IPC-D-356A files use board coordinates unchanged (`.SameCoordinates`). Fab-specific names,
origins and rotation offsets are applied by fab profiles at export (D12).

Design standards used as rule and geometry sources (from the standards themselves, never from another tool's
implementation):

- **IPC-7351B**: land patterns and naming convention (`QFN50P500X500X80-33N`) for generated footprints.
- **IPC-2221B / IPC-2152**: spacing and current-capacity guidance (width/clearance calculators, M8).
- **IPC-6012** classes as rule presets (class 2 / class 3 annular ring etc.).

### KiCad formats

KiCad is GPL. cadlab (MIT) uses it **only as a verification oracle**, plus file-format interop:

- Readers/writers are implemented from KiCad's published file-format documentation and observed files, never from
  KiCad source code.
- Main purpose: export cadlab designs to KiCad so `kicad-cli` can independently run DRC/ERC and generate Gerbers to
  compare against ours (see [TESTING.md](TESTING.md)).
- Import for migration covers boards (`.kicad_pcb`) and netlists. Schematics come in as a netlist exported by
  `kicad-cli`; there is no `.kicad_sch` parser (DECISIONS D13). Netlist import is `circuit.import` (M7):
  components matched to project parts or created as generic, concrete or placeholder parts (DECISIONS D27).
- KiCad's official symbol/footprint/3D libraries are **not** bundled, converted or used as a data source. cadlab
  generates its own (see [PARTS.md](PARTS.md)).

## Fab profiles

Each manufacturer is described by a **fab profile**, a data file (TOML) shipped in-tree and overridable by users.
Capabilities change over time, so every value carries its source URL and the date it was verified. The values
must be filled from each fab's current published capabilities, not from memory.

Built-in profiles live in `fab-profiles/` (embedded in the binary), all verified 2026-10-04 against the fab's own
pages (`fab.show <id>` lists sources and unverified values):

| ID | Processes (layers) | Assembly (BOM/CPL layout) | File names | Unverified / left out (main ones) |
|---|---|---|---|---|
| `jlcpcb` | 1, 2, 4, 6–32 | yes, LCSC SKUs | JLCPCB's | CPL origin |
| `pcbway` | 1–2, 4–14 | yes | generic | CPL columns/origin; drill and copper where its pages disagree |
| `oshpark` | 2 (1 oz, 2 oz), 4, 6 | no service | generic (auto-detected) | drill format; no max drill (milled), no silk height |
| `aisler` | 2 (ENIG, HASL), 4, 6–8 | not from Gerbers | Aisler's table | drill format (asks inch 2:4) |
| `eurocircuits` | 1–2 and 4–8 pooling, PCB proto 2/4 | yes (content only) | generic | hole-to-hole (derived), finishes, multilayer thickness, BOM/CPL headers |
| `seeed` | 1–2, 4, 6 | yes | Seeed's (one drill file) | outer copper, multilayer spacing, 6-layer thickness, names, CPL columns (JS-rendered pages) |
| `nextpcb` | 1–2, 4, 6–32 | yes | generic | min drill, multilayer layers/thickness, BOM/CPL headers; no finish list |
| `pcbgogo` | 1–2, 4–40 | yes | generic | min NPTH, silk height, double-sided assembly, CPL layout; no thickness/copper options |
| `allpcb` | 1–14 (one table) | not published | generic | no thickness/copper options, min size, inner copper |
| `elecrow` | 1–2, 4–8 | yes | Elecrow's | track/space, drill, multilayer rings, names, BOM/CPL headers |
| `generic` | any | — | generic | cadlab's conservative IPC class 2 defaults, for any other fab |

Values a fab does not publish are left out (not checked), never filled in from elsewhere. Users add profiles or override built-in ones with
`*.toml` files in `~/.config/cadlab/fab-profiles/` (`$XDG_CONFIG_HOME/cadlab/fab-profiles/`). A user file whose
`id` (or file stem) matches a built-in profile is merged onto it: tables merge key by key, other values (arrays
included) replace. Code: `src/fab/` (types, loading, `check`, `export`).

```
FabProfile                               fab-profiles/<id>.toml, deny_unknown_fields
├── id, name, website, verified_at ("YYYY-MM-DD"), sources[], notes
├── [[process]]                          preferred first; the first offering the board's layer count is used
│   ├── id, name, layers[]
│   ├── thickness[], outer_copper[], inner_copper[]       lengths (35um = 1 oz); empty = not checked
│   ├── min_track, min_space, min_drill, max_drill, min_npth, min_via_ring, min_pth_ring
│   ├── hole_to_hole (any holes), pad_hole_to_hole (component holes), copper_to_edge
│   ├── min_silk_width, min_silk_height, silk_to_pad, mask_dam
│   ├── via_in_pad, castellated
│   ├── finishes[], mask_colors[], silk_colors[], max_size [a, b], min_size [a, b]
│   └── cite {field = url}, unverified [field, ...]
├── [output]
│   ├── include[]       file kinds or groups: copper, mask, paste, silk, profile, component, drill, ipc356
│   ├── names {kind = template}   copper_top, copper_inner, copper_bottom, mask_*, paste_*, silk_*, profile,
│   │                             component_*, drill_pth, drill_npth, drill_span, ipc356; placeholders
│   │                             {project} {layer} {n} {from} {to}; kinds left out keep cadlab's generic names
│   ├── archive         zip name template ({project}, {fab})
│   ├── drill_format    informative ("excellon": XNC, metric, PTH/NPTH split)
│   └── cite, unverified
└── [assembly]                           optional
    ├── sides[], min_package (chip code "0201"), through_hole, part_classes[]
    ├── catalog[]       supplier provider IDs whose SKUs fill the `sku` BOM column (JLCPCB: lcsc)
    ├── bom  {file, mount_names, columns = [{header, field}]}     fields: line, quantity, designators, value,
    │                                                             description, value_description, package,
    │                                                             footprint, manufacturer, mpn, sku, mount, notes
    ├── cpl  {file, columns, side_names, coordinate_suffix, origin = "board" | "outline_lower_left"}
    │                                                             fields: designator, value, package, footprint,
    │                                                             x, y, side, rotation
    ├── [[rotation_offsets]] {package = "SOT-23*", offset, source}   added to the rotation at export only
    └── cite, unverified ("cpl.origin" names a sub-field)
```

Values that could not be confirmed on the fab's pages are either left out (not checked) or kept and listed in
`unverified`; `fab.show` lists them. Where a fab's own pages contradict each other (PCBWay's capabilities and
tolerances pages), the stricter value is used and marked unverified. No per-package rotation offsets are shipped:
neither fab publishes a table, so they are user data (see D21).

### Commands (`fab.*`)

| Command | What it does |
|---|---|
| `fab.list` | profiles with their processes, origin (builtin, user, merged) and verification date |
| `fab.show {fab}` | a profile in full, with its unverified values and sources |
| `fab.check {fab, process?, parts?, boards?}` | can the fab make and assemble the board: layer count, thickness, copper, finish/color preferences (first offered one is chosen), board size; track width, clearance, hole-to-hole, copper-to-edge and silk-to-pad through cadlab's DRC with a temporary rule set holding the profile's minimums (net class values ignored, the project's rules untouched); drills, annular rings, pad hole-to-hole; silk line width and text height; assembly sides, package size, through-hole; parts availability through the configured suppliers (`fab.no_suppliers` when none). Codes `fab.*`, each with a hint |
| `fab.compare {fabs?, parts?}` | one row per fab: feasible, error/warning counts, failing constraint codes |
| `fab.export {fab, dir?, process?, boards?, force?}` | runs the check (refuses on errors unless `force`), then writes to `out/fab/<fab>/`: the fabrication files named by the profile, the zip archive of them (flat, deflated, fixed timestamps: byte-identical across runs), BOM and CPL in the fab's layouts with rotation offsets applied, and `fab-lock.json` |

`fab-lock.json` records: lock version, generator, project, profile (`id`, `name`, `verified_at`, `source`),
process (`id`, layers, thickness, copper, chosen finish/colors), every file written with its SHA-256 and size
(the archive included, the lock excluded), each populated BOM line with the chosen manufacturer/MPN, the fab SKU
and provider when one of the profile's `catalog` providers offers it and its availability status, and the
rotation offsets applied per designator.

Manifest `targets`: `drc.run` also runs the board and assembly part of `fab.check` for each target and reports
its findings as warnings prefixed `[<fab>]` (never errors), plus `fab.unknown_target` for unknown IDs.

`bom.export --format jlcpcb|pcbway` writes the BOM layout of that profile (without SKUs, which need a supplier
lookup; `fab.export` fills them).

## Provider-agnostic projects

A cadlab project never depends on a manufacturer. Fabs differ in capabilities, lead times and parts stock, and all
of those change, so moving a design to another fab should take one command, not a redesign.

**What the project stores (fab-independent):**

- **Design rules as engineering intent:** track/space, drills, annular rings and clearances the designer chose,
  defaulting to a conservative generic IPC class 2 rule set that mainstream fabs can all make.
- **Board specification as requirements:** layer count, thickness, copper weight, impedance targets, and finish or
  color *preferences* (ordered lists, not a single vendor option).
- **Parts as requirements plus approved MPNs:** a BOM line is a generic requirement (`10k 1% 0402`) and/or a list of
  approved manufacturer part numbers with alternates. Supplier/fab SKUs (e.g. an LCSC `C` number) are cached sourcing
  data, never the identity of a part. See [PARTS.md](PARTS.md).
- **Optional compatibility targets:** `targets = ["jlcpcb", "pcbway"]` in the manifest makes DRC also report
  anything one of those fabs cannot make. This is a hint for checks only, not a binding.

**What is decided at export time, per fab:**

```
project ──► fab check  (capabilities vs design, parts availability vs that fab's catalog/stock)
        ──► fab plan   (chosen process options, resolved SKUs per BOM line, substitutions, warnings, cost estimate)
        ──► export     (files in the fab's layout + fab-lock.json)
```

- `fab.check <id>`: can fab X make this board and assemble this BOM *today*? Reports rule violations,
  unavailable or unsupported parts (substitute candidates drawn from the approved alternates first, then from
  matching generics, are still to come; today the hints point to `bom.approve` / `bom.resolve`).
- `fab.compare jlcpcb,pcbway,...`: side-by-side feasibility (part coverage with `parts`; estimated cost and lead
  time later, where the provider exposes them).
- `fab.export <id>`: writes the fab's files plus a **`fab-lock.json`** recording exactly what was produced: profile
  version, process options, the SKU picked for each BOM line, applied rotation offsets. The lock belongs to the
  export (commit it to reproduce an order), not to the design.
- Fab-specific conventions (CPL rotation offsets, BOM columns, file naming) are applied only at export, from the
  profile. They never leak into the project.

Retargeting from JLCPCB to PCBWay is therefore `cadlab fab check pcbway`, fix what it reports (usually substitute a
few parts), then `cadlab fab export pcbway`.

### Target fabs

Support as many as practical, in this order:

1. **JLCPCB**: PCB + assembly, LCSC parts catalog.
2. **PCBWay**: PCB + assembly.
3. Then (profiles shipped in M7): OSH Park, Aisler, Eurocircuits, Seeed Fusion, NextPCB, PCBgogo, ALLPCB, Elecrow, and a
   **generic IPC class 2 / class 3** profile for any other manufacturer.

Adding a fab should only need a profile file and, if needed, a small output-format adapter (BOM/CPL layout), with no
core code changes.
