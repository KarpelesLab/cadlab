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
| Pick and place | CSV (generic + per-fab column layouts), Gerber X3 | export | M4, generic CSV done (`export.pnp`); per-fab layouts with fab profiles |
| Assembly BOM | CSV / XLSX (generic + per-fab layouts) | export | M1/M4 |
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
  `kicad-cli`; there is no `.kicad_sch` parser (DECISIONS D13).
- KiCad's official symbol/footprint/3D libraries are **not** bundled, converted or used as a data source. cadlab
  generates its own (see [PARTS.md](PARTS.md)).

## Fab profiles

Each manufacturer is described by a **fab profile**, a data file (TOML) shipped in-tree and overridable by users.
Capabilities change over time, so every value carries its source URL and the date it was verified. The values
must be filled from each fab's current published capabilities, not from memory.

```
FabProfile
├── id, name, website, verified_at, sources[]
├── pcb processes[]                     e.g. "standard 2L", "4L/6L", "HDI"
│   ├── layer counts, board thickness options, copper weights
│   ├── min track / space, min drill (mechanical, laser), min annular ring, via-in-pad availability
│   ├── hole-to-hole, copper-to-edge, silk min width/height, mask dam / expansion
│   ├── surface finishes, mask/silk colors, impedance control, castellations, edge plating
│   └── stackups offered (for impedance calculation)
├── output conventions
│   ├── Gerber file naming/extensions, units, format (e.g. 4.6), X2 vs plain RS-274X
│   ├── drill format and plated/non-plated split
│   └── archive layout
└── assembly (optional)
    ├── BOM and CPL column layouts
    ├── rotation/origin conventions and known per-package rotation offsets
    ├── part catalog provider (see PARTS.md) and part classes (e.g. basic vs extended)
    └── assembly constraints (min part size, sides, through-hole support)
```

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

- `fab check --fab <id>`: can fab X make this board and assemble this BOM *today*? Reports rule violations,
  unavailable or unsupported parts, with substitute candidates drawn from the approved alternates first, then from
  matching generics.
- `fab compare --fab jlcpcb,pcbway,...`: side-by-side feasibility, part coverage, estimated cost and lead time
  where the provider exposes them.
- `export fab --fab <id>`: writes the fab's files plus a **`fab-lock.json`** recording exactly what was produced: profile
  version, process options, the SKU picked for each BOM line, applied rotation offsets. The lock belongs to the
  export (commit it to reproduce an order), not to the design.
- Fab-specific conventions (CPL rotation offsets, BOM columns, file naming) are applied only at export, from the
  profile. They never leak into the project.

Retargeting from JLCPCB to PCBWay is therefore `fab check --fab pcbway`, fix what it reports (usually substitute a few
parts), then `export fab --fab pcbway`.

### Target fabs

Support as many as practical, in this order:

1. **JLCPCB**: PCB + assembly, LCSC parts catalog.
2. **PCBWay**: PCB + assembly.
3. Then: OSH Park, Aisler, Eurocircuits, Seeed Fusion, NextPCB, PCBgogo, ALLPCB, Elecrow, and a
   **generic IPC class 2 / class 3** profile for any other manufacturer.

Adding a fab should only need a profile file and, if needed, a small output-format adapter (BOM/CPL layout), with no
core code changes.
