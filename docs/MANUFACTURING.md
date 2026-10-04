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
| Intelligent fab data | **IPC-2581** (rev C) | export | M9, done (`export.ipc2581`) |
| Intelligent fab data | ODB++ | — | not implemented: Siemens' terms do not permit it (below, D37) |
| Routing exchange | **Specctra DSN / SES** | DSN export, SES import (DSN reader as library) | M5, done (`export.dsn`, `route.import_ses`; [ROUTER.md](ROUTER.md)) |
| Mechanical CAD exchange | **IDF 3.0**, **IDX** (ProSTEP EDMD) | export | M9, done (`export.idf`, `export.idx` baseline) |
| 3D | **STEP** AP214/AP242 (export), 3D models (import) | both | M9: AP214 export done (`export.step`); models via oxideav-mesh3d (STL, OBJ, glTF/GLB, USDZ now; STEP/VRML decoders pending upstream, D36) |
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

`export.all` also writes `<project>-ipc2581.xml` (below). Gerber, drill, IPC-D-356A and IPC-2581 files use
board coordinates unchanged (`.SameCoordinates`). Fab-specific names,
origins and rotation offsets are applied by fab profiles at export (D12).

### Exchange outputs: IPC-2581, STEP, IDF, IDX (M9)

All four are deterministic (fixed or no dates), in board coordinates (mm, Y up), and written by hand from the
published specifications with no new dependency. Algorithm code: `src/fabout/ipc2581.rs` and `src/mcad/`
(STEP, IDF and IDX share the outline loops, hole list and component bodies built in `mcad`).

**IPC-2581 revision C** (`export.ipc2581 [path] [--mask-expansion]`, default `out/fab/<project>-ipc2581.xml`,
also part of `export.all`). One XML file, namespace `http://webstds.ipc.org/2581`, function mode `ASSEMBLY`:

| Section | Content |
|---|---|
| `Content` | step and layer references, BOM reference; `DictionaryLineDesc` (`LINE_<width>`, round ends) and `DictionaryStandard` (`Circle`, `RectCenter`, `RectRound`, `Oval`; ids name shape and size: `RECT_1X0.6`, `RRECT_0.565X0.57R0.14125`) |
| `LogisticHeader` | sender role, enterprise and person as placeholders (`UNKNOWN`): projects store no people or companies |
| `Bom` | one `BomItem` per BOM line (part ID as `OEMDesignNumberRef`, quantity, pin count, description), `RefDes` per designator (`populate="false"` for DNP), value, manufacturer, MPN and package as `Textual` characteristics |
| `Ecad/CadData` | layers in stack order (`F.SilkS`, `F.Paste`, `F.Mask`, copper and `DielectricN`, ..., `B.SilkS`), one `DRILL_<from>_<to>` layer per copper span with its `Span`; `Stackup` with copper thicknesses and dielectrics sharing the rest of the board thickness (`whereMeasured="METAL"`) |
| `Step` | `Datum`, `Profile` (outline polygon with `PolyStepCurve` arcs, `Cutout`s), a `Package` per footprint (courtyard outline, pick-up point, `LandPattern` pads, body rectangle as `AssemblyDrawing`, `Pin`s), a `Component` per placement (`Xform` rotation, `mirror` on the bottom: mirror about Y, then counter-clockwise rotation), `LogicalNet`s (component pins by pad number), and `LayerFeature`s |
| Features | copper: pads, vias (`padUsage="VIA"`), tracks (`Line`, `Arc`) and zone fills (`Contour` with `Cutout`s) grouped in a `Set` per net; mask: pad openings grown by the mask expansion (vias tented); paste: SMD pads or exposed-pad windows; legend: footprint silk, designators and board graphics as strokes (`Polyline`, `Line`, `Arc`), not clipped at mask openings; drill: `Hole`s with `PLATED`, `NONPLATED` or `VIA` |

Left out: `HistoryRecord` and `Avl` (both require dates; the AVL would repeat the BOM's MPNs), `PadStackDef`
(pads are written per layer instead), material `Spec`s (the project has requirements, not materials), board
graphics on copper, mask and paste layers. The schema (`IPC-2581C.xsd`) is published by IPC at
`webstds.ipc.org` (not reachable without access at the time of writing) and is not shipped: its distribution terms
are IPC's. Element order follows the published revision C structure and `kicad-cli pcb export ipc2581` output
(observed, as an oracle). Tests parse the file back and check structure, references and counts against the
board; `CADLAB_IPC2581_XSD=/path/IPC-2581C.xsd` with `CADLAB_ORACLES=1` validates with `xmllint` (docs/TESTING.md).

**STEP** (`export.step [path] [--vias] [--components false]`, default `out/mcad/<project>.step`): ISO 10303-21,
schema AP214 (`AUTOMOTIVE_DESIGN`), millimeters. An assembly product named after the project holds the board part
and one body part per footprint, instanced per designator (`NEXT_ASSEMBLY_USAGE_OCCURRENCE` with the designator as
name, placement through `ITEM_DEFINED_TRANSFORMATION`). Geometry is exact B-rep (`MANIFOLD_SOLID_BREP`): the board
is the outline extruded from Z = 0 to the stackup thickness, with planar faces for straight edges, cylindrical
faces for arcs, outline cutouts and drilled holes (pad and mounting holes; vias with `vias`). Holes crossing the
edge, a cutout or a larger hole are not cut (`export.step_hole_skipped`, with location). Bodies are boxes from
the footprint's package dimensions (`body`: width, length, height; generated footprints have them), centered on
the footprint origin, on the top face, or turned over under the bottom face for bottom-side parts. Components
without a body are reported (`export.no_body`); DNP parts are left out. Colors: green board, dark gray bodies.
The header time stamp is fixed (`1970-01-01T00:00:00`).

Components with an attached 3D model (`footprint.model_set`, see [PARTS.md](PARTS.md#3d-models)) get the model
instead of the box. The model is a triangle mesh (decoded through oxideav-mesh3d, D36), written as a **faceted
B-rep**: vertices welded on the nanometer grid, one `FACE_SURFACE` per triangle on its `PLANE`, bounded by a
`POLY_LOOP`. Each connected piece that is closed and consistently wound becomes a `FACETED_BREP` (`CLOSED_SHELL`,
turned outward by the sign of its volume) in a `FACETED_BREP_SHAPE_REPRESENTATION`, so MCAD tools see solids
(the FreeCAD oracle checks validity and volume). A model with an open or inconsistently wound piece is written
as surfaces instead (`SHELL_BASED_SURFACE_MODEL` with `OPEN_SHELL`s in a `MANIFOLD_SURFACE_SHAPE_REPRESENTATION`)
and reported (`export.step_open_model`, info). Colors are the model's material colors: the most common one per
piece, plus face styles where faces differ. One body part per footprint and model reference, named
`<footprint>_<model file>`, placed like the boxes (bottom side: turned over). A model that cannot be read is
reported (`model.invalid`, `model.unsupported_format`, ...) and the package box is used.

**IDF 3.0** (`export.idf [dir] [--vias] [--components false]`, default `out/mcad/`): `<project>.emn` (board:
`.HEADER` with units `MM`; `.BOARD_OUTLINE ECAD` with the thickness and loops of points, label 0 the outline
counter-clockwise, then cutouts clockwise, arcs as included angles, circles as center + point at 360°;
`.DRILLED_HOLES` with `PTH`/`NPTH`, associated designator or `BOARD`, type `PIN`/`VIA`/`MTG`, owner `ECAD`;
`.PLACEMENT` with geometry = footprint name, part number = MPN or part ID, designator, position, rotation,
`TOP`/`BOTTOM`, `PLACED`) and `<project>.emp` (library: one `.ELECTRICAL` outline per geometry and part number,
the body rectangle with its height; for a component with a 3D model, the model's bounding rectangle in footprint
coordinates and its top as the height). Bottom-side parts: the library outline mirrored about its Y axis, then
rotated counter-clockwise, as cadlab places them (for the centered body rectangles any mirror axis gives the
same result). Header dates are fixed (`1970/01/01.00:00:00`).

**IDX** (`export.idx [path] [--vias] [--components false]`, default `out/mcad/<project>.idx`): the ProSTEP iViP
PSI 5 "ECAD/MCAD Collaboration" format, written from the free V4.5 recommendation, implementation guidelines
and schema (prostep ivip, `https://www.prostep.org/fileadmin/prod-download/PSI5_IDXv4.5_release.zip`, schema
namespaces `http://www.prostep.org/ecad-mcad/edmd/4.0/...`). The recommendation is "available for anyone to
use" and may be duplicated "for use in the context of creating software"; its schema is not shipped (it may only
be redistributed unchanged with its notice; tests take it from `CADLAB_IDX_XSD`). One `EDMDDataSet` with a
`SendInformation` process instruction: the **baseline** that starts (or resets) a collaboration. Change and
response messages (`SendChanges`) are not written or read yet.

| Item (`GeometryType`) | Shape | Placement |
|---|---|---|
| `BOARD_OUTLINE` | `Stratum` (`DesignLayerStratum`, `PrimarySurface`): the outline curve (polylines, arcs with included angles, or a circle) from Z 0 to the thickness, cutouts as inverted shape elements; `THICKNESS` property | none (board coordinates) |
| `HOLE_PLATED`, `HOLE_NON_PLATED`, `VIA` (vias with `vias`) | `InterStratumFeature` (`PlatedCutout`, `Cutout`, `Via`) on the board stratum: a circle of the finished diameter through the board, inverted; one padstack item per kind and diameter (`PTH_1`, `NPTH_2.2`) | 2D transform at the hole; `PADSTACK` property |
| `KEEPOUT_AREA_ROUTING`, `_VIA`, `_OTHER` | `KeepOut` with purpose `Route`, `Via`, `Plane` (a cadlab keep-out's `no_tracks`, `no_vias`, `no_pours`), Z 0 to the thickness; the layers in the description | none |
| `KEEPOUT_AREA_COMPONENT` | `KeepOut` (`ComponentPlacement`) for `no_footprints`, one per side its copper layers touch (all layers: both): from the top face up, or the bottom face down, unbounded | none; `SIDE` property |
| `COMPONENT` | `AssemblyComponent` (`Physical`): the package body rectangle from Z 0 to the body height, one item per footprint and part number with `PackageName` (footprint), `PARTNUM` (MPN or part ID) and `HEIGHT` | 3D transform: on the top face rotated counter-clockwise; on the bottom face turned over (X → -X, Z → -Z), then rotated, so the body hangs below; `REFDES`, `SIDE` properties |

Every top-level item is an `assembly` item with the IDX 4.0 `GeometryType` attribute and one instance of a
`single` item whose shape is the classic classification object (both methods of the guidelines at once, so
readers of either work). Identifiers use system scope `CADLAB` and stable numbers from names (`BOARD`,
`HOLE:J1.1`, `HOLE:H1`, `VIA:<id>`, `KEEPOUT:<name>:ROUTING`, `COMPONENT:U1`, `PACKAGE:<footprint>:<part>`), so
a later baseline names the same objects the same way. Z = 0 is the board's bottom face (the IDX convention);
matrices are `x' = xx·x + xy·y + xz·z + tx`. Creator name and company are empty (projects store no people), the
creation and modification time stamps fixed (`1970-01-01T00:00:00Z`). Bodies come from the attached 3D
model's bounding box when there is one (D36), else from the footprint's package body; components with neither
are reported (`export.no_body`) and left out, DNP parts too. Model files are not referenced (`Model3D`). Checked by the published XSD with `xmllint` (oracle above).

**ODB++ is not implemented.** The ODB++ Design Format Specification (Siemens, release 8.1 update 4, August 2024,
`https://odbplusplus.com/wp-content/uploads/sites/2/2024/08/odb_spec_user.pdf`) is free to download, but its
notice reads: "This Documentation contains trade secrets or otherwise confidential information owned by Siemens
[...] This Documentation may not be copied, distributed, or otherwise disclosed by Customer without the express
written permission of Siemens, and may not be used in any way not expressly authorized by Siemens." The
format description v7 (Mentor Graphics,
`https://odbplusplus.com/wp-content/uploads/sites/2/2020/03/ODB_Format_Description_v7.pdf`) states that downloading the specification "does not grant a license to develop software
interfaces based on the format specification"; developers are directed to the ODB++ Solutions Development
Partnership, whose terms (`https://odbplusplus.com/design/?p=712000`) grant "a nontransferable, nonexclusive
license to use the ODB++ Format solely to develop, test and support an interface with the Participant
Products", without sublicensing, against promotional obligations and terminable by Siemens. A license that
cannot pass to the users and forks of an MIT crate is not compatible with cadlab's license, and implementing the
format from observed files (KiCad's exporter, for instance) would sidestep the same terms. IPC-2581 (an open
IPC standard, `export.ipc2581`) covers the same intelligent-fab-data need. See DECISIONS D37.

Not yet: generated package bodies other than boxes in STEP (pins, chamfers, cylinders; the 3D view has them),
STEP/VRML model files (they arrive with the oxideav STEP/VRML decoders, D36), writing a STEP model's exact B-rep
through (models are meshes), AP242, IDF `.PLACE_OUTLINE`/keep-outs, IDX change and response messages.

Design standards used as rule and geometry sources (from the standards themselves, never from another tool's
implementation):

- **IPC-7351B**: land patterns and naming convention (`QFN50P500X500X80-33N`) for generated footprints.
- **IPC-2221B / IPC-2152**: spacing and current-capacity guidance (width/clearance calculators, M8).
- **IPC-6012** classes as rule presets (`board.rules --preset ipc2|ipc3`; values, sources and their unverified
  status in [BOARD.md](BOARD.md), "Rule presets").

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
| `fab.check {fab, process?, parts?, boards?}` | can the fab make and assemble the board: layer count, thickness, copper, finish/color preferences (first offered one is chosen), board size; track width, clearance, hole-to-hole, copper-to-edge and silk-to-pad through cadlab's DRC with a temporary rule set holding the profile's minimums (net class values ignored, the project's rules untouched); drills, annular rings, pad hole-to-hole; silk line width and text height; assembly sides, package size, through-hole; parts availability through the configured suppliers (`fab.no_suppliers` when none), at the profile's `catalog` providers when configured, with the substitutions of the fab's `fab-lock.json` (`dir`, default `out/fab/<fab>`) applied (`fab.substituted` info) and ranked substitute candidates (`candidates`, default 3) for every unavailable line in the output's `substitutes` and in the finding's hint. Codes `fab.*`, each with a hint |
| `fab.compare {fabs?, parts?}` | one row per fab: feasible, error/warning counts, failing constraint codes |
| `fab.export {fab, dir?, process?, boards?, force?}` | runs the check (refuses on errors unless `force`), then writes to `out/fab/<fab>/`: the fabrication files named by the profile, the zip archive of them (flat, deflated, fixed timestamps: byte-identical across runs), BOM and CPL in the fab's layouts with rotation offsets applied, and `fab-lock.json`; substitutions in the previous lock are used and kept, substitute candidates are reported as in `fab.check` |
| `fab.substitute {fab, part, mpn?, remove?, force?, boards?, dir?}` | applies a substitute for one BOM line at one fab: `mpn` (default the best candidate) must be one of the line's candidates (`bom.substitutes`) unless `force` (basis `manual`). Writes it to the fab's `fab-lock.json` (creating a lock without files if there was no export yet), never to the project; `remove` undoes it. The next `fab.check` / `fab.export` for that fab order the substitute; other fabs are not affected |

`fab-lock.json` records: lock version, generator, project, profile (`id`, `name`, `verified_at`, `source`),
process (`id`, layers, thickness, copper, chosen finish/colors), every file written with its SHA-256 and size
(the archive included, the lock excluded), each populated BOM line with the chosen manufacturer/MPN, the fab SKU
and provider when one of the profile's `catalog` providers offers it, its availability status and, for a
substituted line, the MPN it `replaces`, the rotation offsets applied per designator, and the `substitutions`
chosen for this fab (`part`, `replaces`, `manufacturer`, `mpn`, `provider`, `sku`, `basis`: `drop_in`,
`parametric` or `manual`), sorted by part. A lock written by `fab.substitute` before any export has only the
header and the substitutions.

Design rules can be derived from a profile with `board.rules --fab <id> --margin tightest|comfortable`
([BOARD.md](BOARD.md), "Design rules"): only the resulting numbers are stored in the project.

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
  unavailable or unsupported parts. Approved alternates are tried first (they are part of the line); for a
  line still unavailable at the fab's catalog, it reports ranked substitute candidates: drop-ins from
  providers' cross-reference data, and for passives parts matching the requirement (same package and value,
  tolerance at most and ratings at least the part's). See [PARTS.md](PARTS.md), "Substitutes".
- `fab.substitute <id> <part> [mpn]`: the substitution report becomes a choice for that fab only, recorded in
  its `fab-lock.json`; `bom.approve` instead adds an alternate for every fab (a design change).
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
