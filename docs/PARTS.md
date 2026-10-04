# Parts, libraries and BOM

## Part model

A **part** bundles everything needed to use a component in a design:

```
Part
├── id                 library-unique, e.g. "ldo/AP2112K-3.3"
├── kind               Generic | Concrete
├── manufacturer, mpn  (Concrete only)
├── category           resistor, capacitor, ldo, mcu, connector, ...
├── parameters         typed: resistance=10k, tolerance=1%, voltage_out=3.3V, package=SOT-23-5, ...
├── symbol             pins: number, name, electrical type (in/out/bidir/power_in/power_out/passive/nc/...),
│                      graphical units for multi-unit parts; drawing optional (auto-generated from pins)
├── footprints         one or more candidates; pin → pad mapping per footprint
├── model3d            optional, per footprint: a 3D model file (FootprintRef.model; see "3D models")
├── datasheet          URL (+ cached local copy path)
├── sourcing           supplier offers: supplier, SKU, stock, price breaks, MOQ, lifecycle, fetched_at
└── provenance         where it came from (generated, supplier, user import, hand-written) + license
```

**Generic** parts describe a requirement (`resistor 10k 1% 0402`). They are enough for circuit design and layout,
since the footprint is known from the package. **Concrete** parts carry an MPN.

A BOM line holds the requirement and, optionally, an ordered list of **approved MPNs** (primary + alternates). It
does not hold a supplier or fab SKU as its identity. Resolution to an orderable SKU happens **per fab, at export**
(`fab check` / `export fab`), picking the first approved MPN the fab or its supplier has in stock, then falling
back to any part matching the generic requirement, and reporting every substitution. The same project can
therefore be built at JLCPCB one month and PCBWay the next. See
[MANUFACTURING.md](MANUFACTURING.md#provider-agnostic-projects).

## Libraries

- **Project library** (`library/` in the project): every part used by the project is copied here. A project never
  breaks because an external library changed.
- **Shared user libraries** (`lib.*` commands, DECISIONS D19): parts, footprints and blocks kept outside any
  project and copied in and out explicitly. Projects never read them implicitly; when a missing part exists in a
  shared library, the `part.not_found` error hints `lib.import <id>`.
- **Generated**: footprints and symbols created from parameters (below). This is the main source: cadlab ships
  its own base library built from generators, never from KiCad's libraries (DECISIONS D7).
- **User imports**: a user's own KiCad `.kicad_sym` / `.kicad_mod` files can be imported (below, D41). The
  user is responsible for the license of what they import, which is recorded in provenance.

### Shared user libraries

Locations, in search order:

1. The **user library**: `$XDG_DATA_HOME/cadlab/library`, default `~/.local/share/cadlab/library`
   (`%APPDATA%\cadlab\library` on Windows when `XDG_DATA_HOME` is unset). Created on first publish.
2. Directories listed in the user settings (`~/.config/cadlab/config.toml`), e.g. a team library kept in a git
   checkout: `libraries = ["~/hw/team-library"]`. Each is named after its directory (`team-library`).
   `cadlab config show` prints the list.

The `library` argument of the commands takes a library name (`user`, `team-library`) or a directory path
(`./lib`), which need not be configured.

Layout: the same as a project's `library/`, plus blocks. Files use the canonical JSON writer.

```
<library>/
├── library.toml              schema_version = 1
├── parts/<id>.json           same format as a project part
├── footprints/<name>.json    same format as a project footprint
├── models/<file>             3D model files (`sot23-5.stl`), as-is
└── blocks/<name>.json        {"name", "block", "parts", "footprints", "models"}
```

A block file is **self-contained**: it carries copies of every part its components use and of their footprints,
so importing it into an empty project always works, whatever happens later to the library's `parts/`.

| Command | Purpose |
|---|---|
| `lib.list [query] [--kind part\|footprint\|block\|model] [--library L]` | items across libraries, with the library each comes from; no project needed |
| `lib.show <name>` | one item in full |
| `lib.publish --part ID \| --footprint NAME \| --block NAME [--library L] [--replace]` | copy from the project into a library: a part brings its footprints, a block its parts and footprints |
| `lib.import <name> [--kind K] [--library L] [--replace]` | copy into the project (part + footprints; block + parts + footprints), reporting what was added |
| `lib.remove <name> [--library L]` | delete from a library (default: the user library); a footprint still used by a library part is kept |

Without `library`, `lib.import` and `lib.show` take the first library that has the name (an earlier library
shadows a later one); parts are also found by MPN. Existing items with different content give a `lib.conflict`
error listing them; `replace: true` overwrites them. Identical items are reported `unchanged`. Library writes
(`publish`, `remove`) happen outside the project: `--dry-run` reports without writing, and undo does not revert
them.

## Footprint generation (IPC-7351B)

Many standard packages can be computed instead of looked up, which helps agents a lot: given datasheet
dimensions, cadlab produces a correct land pattern (`src/landpattern/`).

- Pads from the IPC-7351B equations (Z/G/X with toe/heel/side fillets, RMS tolerance stacking, F = 0.05 mm,
  P = 0.025 mm, 0.01 mm rounding; all configurable), at Most/Nominal/Least density.
- Families today: chip (0201–2512, resistor/capacitor/inductor/LED/diode/fuse), gull-wing two-row (SOIC, SOP,
  TSSOP, MSOP, SOT-23-3/5/6 with unpopulated slots), QFP, DFN/SON, QFN (exposed pad with paste windows), THT pin
  headers, leads + tab (SOT-223 with tab = pad 4; DPAK/TO-252 and D2PAK/TO-263 with leads 1, 3 and tab = pad 2,
  the cut middle lead), SOD (SOD-123/323 gull-wing, SOD-123F/523 flat lead `SODFL`), MELF/MiniMELF, molded
  bodies (SMA/SMB/SMC, molded tantalum `CAPMP`), DIP (300/600 mil, pin 1 square, counter-clockwise), BGA
  (collapsing balls, JEDEC row letters, depopulated list or center void). Diodes: pad 1 = cathode, on the left.
  Planned: terminal blocks.
- Outputs: pads, paste windows, courtyard, silkscreen clipped around pads (via polyclip), fab outline with pin-1
  chamfer, pin-1 dot, body dimensions for 3D.
- Names follow IPC-7351 (`RESC1005X40N`, `SOIC127P600X175-8N`, `QFN50P500X500X90-33N`). Common names (`0402`,
  `SOT-23-5`, `SOIC-8`, `TSSOP-20`, `LQFP-48`, `QFN-32 5x5mm P0.5mm EP3.1mm`, `PinHeader 2x05`) map to typical
  JEDEC/EIA dimensions (`landpattern::packages`); for anything else, pass the datasheet dimensions as a
  `PackageSpec` (`"0.15..0.35mm"`, `"1.0±0.05mm"`).
- **To verify:** the fillet goal table (`src/landpattern/ipc.rs`) follows IPC-7351B as best known; review it
  against the standard. A SOIC-8 computed with it matches widely used IPC-derived footprints to 0.01 mm. Newer
  rows to review: flat lead, molded body (outer/inner goals mapped from IPC's heel/toe at the bend), MELF, the
  BGA land reduction (25/20/15 % by ball size, same land at all densities) and BGA courtyard; also the typical
  dimensions for SOD-123F, SOD-523, DPAK/D2PAK tab solderable length and DIP body sizes in `packages.rs`.
- Parametric BGA names: `BGA-64 8x8 P0.8mm 6x6mm` (pins, columns x rows, pitch, body), optional `B0.45mm` ball,
  `H1.2mm` height, `VOID2x2` empty center block; the pin count must match the populated balls.

## Symbol generation

From a pin table (which an agent can extract from a datasheet), `symbolgen` lays out a box symbol on a 2.54 mm
grid: ground pins at the bottom, supply inputs at the top, inputs and bidirectional pins left, outputs right,
connector pins left in number order. Ports (`PA*`, `PB*`, or explicit `group`s) stay together and are moved
between sides to balance them; explicit `side`s are kept. Two-terminal parts get fixed styles (resistor,
capacitor, inductor, diode/LED with pin 1 = cathode, ...). Multi-unit parts are drawn as one body for now:
pins carry their `unit` (from imported KiCad symbols) and each unit's pins stay together on their side.

## Generic part specs

`R 10k 1% 0402`, `C 100nF 16V X7R 0402`, `L 4.7uH 1A 0805`, `FB 600R 0603`, `LED red 0603`: type, value and
package, plus optional tolerance, voltage/current/power rating, dielectric, color. The spec gets a canonical
form and ID (`R_10k_1pct_0402`), so the same requirement written differently reuses the same part.

## Commands

| Command | Purpose |
|---|---|
| `part.generic <spec>` | add a generic passive with generated symbol and footprint |
| `part.create` | concrete or custom part from a pin list + package name or dimensions |
| `part.list / show / set / remove` | inspect and edit the library (removal refused while in use) |
| `footprint.generate / list / show / remove` | land patterns |
| `footprint.set` | local settings of a footprint or some of its pads: mask and paste margins, clearance, zone connection, net ties, pads on the back, mask openings per side, slots, paste-in-hole ([BOARD.md](BOARD.md), "Local settings, net ties and custom rules") |
| `footprint.model_set / model_clear / model_list` | 3D models of footprints and parts (below) |
| `footprint.import_kicad <path>` | the user's KiCad footprints (`.kicad_mod`, `.pretty`) into the project (below) |
| `part.import_kicad_sym <path>` | the user's KiCad symbols (`.kicad_sym`) as project parts (below) |
| `lib.import_kicad <path>` | either into a shared library instead |
| `circuit.add <part or spec>` | add components (`--count`), auto-numbered by category (R1, C3, U2) |
| `circuit.remove / list` | components |
| `bom.list` | one line per part: quantity, designators, DNP, order MPN, unsourced lines |
| `bom.approve <part>` | approved MPNs: alternates (concrete parts) or candidates (generic parts) |
| `bom.dnp <refdes>` | do not populate (stays on the board, excluded from assembly) |
| `bom.note <part> <text>` | purchasing/assembly note |
| `bom.replace <from> <to>` | switch components to another part, warning about missing pins |
| `bom.export <path> --format generic|jlcpcb|pcbway` | CSV; fab layouts omit DNP and warn about lines without MPN |

### Importing KiCad libraries

Your own KiCad 6+ libraries (DECISIONS D41; KiCad's shipped libraries are never read or converted, D7):

```sh
cadlab footprint import-kicad MyLib.pretty [--footprint SOT23_X] [--license "CC-BY-SA-4.0"]
cadlab part import-kicad-sym MyLib.kicad_sym --footprints MyLib.pretty [--symbol LDO_X] [--category ldo]
cadlab lib import-kicad MyLib.kicad_sym --footprints MyLib.pretty [--library team-library]
```

- **Footprints** (`.kicad_mod`, or every `.kicad_mod` of a `.pretty` directory) are converted like the
  footprints of an imported board (D32): named after the file, pads of every shape (custom pads as polygon
  pads; trapezoids as their bounding box, chamfers as rounded corners, each with an
  `import.pad_approximated` warning; oval holes as slots), drills, paste (paste-only apertures become paste
  windows or paste drawings), local settings (mask and paste margins, clearances, zone connections, net ties,
  pads on the back, per-side mask openings, paste-in-hole; D40), copper, mask and paste drawings of either
  side, silkscreen, fab and courtyard drawings of the footprint's side (arcs as polylines; a courtyard made of lines, a
  rectangle or a circle becomes the courtyard polygon; without one, a box 0.25 mm around the pads). Not kept,
  each reported: texts (`import.footprint_text`), drawings on other layers, inner-layer copper and board edges
  (`import.footprint_layer`), 3D model paths (`import.footprint_model`: attach a model with
  `footprint.model_set`). An `at` in a
  library file is ignored, as KiCad does.
- **Symbols** (`.kicad_sym`, all or `--symbol` ones) become parts named after the symbol. Pins keep number,
  name (`~` = none), electrical type (`free` reads as passive), the side of the body they are on (from the
  KiCad orientation), their unit for multi-unit symbols (unit 0 or pins repeated in every unit are shared;
  De Morgan body styles are the same pins) and their alternate functions. Derived symbols (`extends`) get the
  root symbol's pins and their own fields; the standard ones (Reference, Value, Footprint, Datasheet,
  description, keywords) fall back to the parent's, as in KiCad. **The drawing is not converted**: cadlab
  symbols are generated from their pins (box or two-terminal style, D41), keeping each pin's side.
- **Fields:** the category comes from the reference prefix (`R`, `C`, `L`, `FB`, `D`/`LED`, `Q`, `U`, `J`,
  `Y`, `X`, `SW`, `F`, `TP`, `H`), refined by name, value, description and keywords (`LDO`, `MOSFET`, ...), or
  from `--category`. Value becomes the resistance/capacitance/inductance/impedance/frequency or LED color
  when it reads as one; Datasheet (`~` = none), Description (or `ki_description`), MPN
  (`MPN`, `Manufacturer Part Number`, `Mfr. Part #`, ...) and manufacturer fields are kept; other fields become
  parameters (`Voltage Out` → `voltage_out`, typed when the key is known). Distributor and assembler fields
  (`LCSC`, `DigiKey_PN`, `Mouser`, ...) are dropped with `import.symbol_supplier_field`: projects stay
  provider-agnostic (D12). `Sim.*` fields are dropped (`import.symbol_sim_field`).
- **Footprint field:** `MyLib:SOT23_X` attaches footprint `SOT23_X` when the target library has it (imported
  with `--footprints` in the same command, or before) and it has a pad for every pin; else
  `import.symbol_footprint_missing` / `import.symbol_pin_without_pad` and the part has no footprint.
- **Skipped**, listed in `skipped`: power symbols (`import.symbol_power`: nets in cadlab, drawn as power
  symbols on export), symbols without pins (`import.symbol_no_pins`), broken `extends`
  (`import.symbol_extends`). Hidden power input pins, which KiCad joins to the net of their name, get
  `import.symbol_hidden_power_pin`: cadlab connects pins only explicitly.
- **Conflicts:** an item equal to the target's is `unchanged`; a different item of the same name (or case
  variant) is an `import.conflict` error listing them all, unless `--replace` (a 3D model set in cadlab on a
  replaced footprint is kept). Replacing a part used in the circuit that loses pins warns
  (`import.part_in_use`). Project imports are undoable and `--dry-run` reports without changing anything;
  `lib.import_kicad` writes outside the project like `lib.publish` (nothing in a dry run, no undo). KiCad 5
  files (`(module`, `.lib`) are refused with `import.kicad_version` and the `kicad-cli fp/sym upgrade`
  command to run first.

### 3D models

A footprint can carry a 3D model used by `render.board3d`, `export.step` and `export.idf` instead of the body
generated from its package dimensions; a part can carry its own model for one of its footprints (an LED and a
resistor on the same `0603` land pattern), which wins over the footprint's. Models are files decoded by the
**oxideav-mesh3d** crate family (DECISIONS D36): STL, Wavefront OBJ, glTF 2.0 (`.gltf` with embedded buffers,
`.glb`) and USDZ today; STEP and VRML once the oxideav STEP/VRML decoders are published. cadlab has no mesh
parser of its own.

```sh
cadlab footprint model-set SOT95P280X145-5N models/sot23-5.stl
cadlab footprint model-set LEDC1608X80N led.glb --part LED_red_0603 --rotation 0,0,90
cadlab footprint model-set CONN_USB_C usb-c.obj --unit mm --up z --offset 0mm,-1.2mm,0mm --scale 1
cadlab footprint model-list                  # files, users, triangles, extents, readable formats
cadlab footprint model-clear SOT95P280X145-5N
```

- `footprint.model_set` copies the file into the project library, `library/models/<name>` (stored as-is; the
  name keeps the extension, which selects the decoder), decodes it to validate it, and stores the reference on
  the footprint (or on the part's footprint reference with `part`). `file` is a path (relative to the project) or
  the name of a model already in the library. A different file under an existing name is a `model.exists`
  conflict unless `replace: true`. `--dry-run` decodes and reports without changing anything; undo reverts the
  file and the reference together.
- The reference is exact: `offset` (X, Y, Z in `Nm`, Z up from the board), `rotation` (about X, then Y, then Z,
  millidegree `Angle`s), `scale` (one or three factors, exact to 1 ppm), `unit` (`mm`, `cm`, `m`, `in`, `mil`,
  `ft`) and `up` (`y` or `z`) when the file's own are wrong (STL is read as mm with Z up, glTF and OBJ as m with
  Y up). Model coordinates are converted to mm, turned so `up` is +Z (Y up: the model's front, +Z, faces the
  board's front edge), scaled, rotated, then offset: the result is in footprint coordinates (IPC zero
  orientation). A model whose extent is more than 10 times off the package body gets `model.size_mismatch`.
- Errors: `model.unsupported_format` (with the formats available now; `.step`/`.wrl` say the decoders are pending
  upstream), `model.invalid`, `model.empty`, `model.file_not_found`, `model.too_large` (64 MiB), `model.bad_scale`,
  `model.part_footprint`, `model.not_set`. Renders and exports never fail on a model: they fall back to the
  generated body with a warning.
- Model files nothing references are removed with the last reference (`model_clear`, `footprint.remove`,
  `part.remove`); regenerating a footprint keeps its model.
- Shared libraries carry models: `lib.publish` of a part or footprint also writes its model files to
  `<library>/models/`, `lib.import` copies them back, a block file embeds them (base64), and `lib.remove` keeps a
  model a library footprint or part still uses. Model files are library items of kind `model`.
- Without the default `models3d` cargo feature, references and files are kept and copied, but nothing is decoded.

Fab column layouts move to fab profiles in M4 (they follow the fabs' current templates; verify before
ordering). Research commands are below.

## Research providers

Research lives in `src/supplier/`. Providers are synchronous (network ones do blocking I/O, D14):

```rust
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn search(&self, q: &SearchQuery) -> Result<Vec<Candidate>, ProviderError>;  // keywords + filters
    fn lookup(&self, mpn: &str) -> Result<Vec<Candidate>, ProviderError>;        // exact MPN
}
```

A `Candidate` is an orderable offer: provider and SKU (e.g. an LCSC `C` number), manufacturer and MPN,
package, parameters normalized to cadlab keys, stock, MOQ, price breaks (exact `Money`), lifecycle, datasheet, and `drop_in`: MPNs the provider's cross-reference data lists as drop-in replacements.
`Suppliers` queries every configured provider, filters with the query, and ranks: in stock, active, cheapest
at the needed quantity, most stock. A failing provider is reported as a warning; the others still answer.

**Configured today:**

- **Catalog files** (offline): JSON lists of candidates (format in `src/supplier/catalog.rs`): a stock list, a
  parts drawer, or data exported from a distributor. Loaded from `~/.config/cadlab/catalogs/*.json` and the
  paths in `CADLAB_CATALOGS`.
- **DigiKey** (Product Information API v4, client-credentials OAuth): run `cadlab config digikey` once; it asks
  for the client ID and secret of your DigiKey API app, checks them with DigiKey and stores them in
  `~/.config/cadlab/config.toml` (owner-only). Optional `--site` (US), `--currency` (USD), `--language` (en),
  `--sandbox`. Environment variables `DIGIKEY_CLIENT_ID`, `DIGIKEY_CLIENT_SECRET`, `DIGIKEY_SITE`,
  `DIGIKEY_LANGUAGE`, `DIGIKEY_CURRENCY`, `DIGIKEY_SANDBOX=1` override the file. One candidate per packaging (cut tape, tape & reel; Digi-Reel skipped), with
  parameters normalized to cadlab keys (`src/supplier/normalize.rs`). `cargo test --test digikey_live` checks
  the integration against the real API when credentials are set.
- **Mouser** (Search API v1, API key; `src/supplier/mouser.rs`): request a Search API key at
  <https://www.mouser.com/api-search/> (free, with a Mouser account), then run `cadlab config mouser`: it asks
  for the key without echo, checks it with a one-record search and stores it. `MOUSER_API_KEY` overrides the
  file. Uses `POST /api/v1/search/keyword` (keywords and MPN lookups; exact MPN matches are kept, since
  `search/partnumber` takes Mouser part numbers). One candidate per Mouser part number: stock
  (`AvailabilityInStock`), MOQ (`Min`), price breaks (in the key's account currency), lifecycle
  (`LifecycleStatus`, `IsDiscontinued`; an empty status stays `unknown`), datasheet, category, and
  `ProductAttributes` normalized like DigiKey's (`Package / Case` gives the package). Mouser allows 30 calls a
  minute and 1000 a day. Source: Mouser's OpenAPI description <https://api.mouser.com/api/docs/V1> (UI:
  <https://api.mouser.com/api/docs/ui/index>).
- **Nexar / Octopart** (supply GraphQL API, client-credentials OAuth; `src/supplier/nexar.rs`): create an
  application with the Supply scope at <https://portal.nexar.com> (plans and part quotas:
  <https://nexar.com/compare-plans>; the free plan's quota is small and may not include pricing), then run
  `cadlab config nexar` (client ID, secret without echo, optional `--country` (US), `--currency` (USD),
  `--unauthorized`); it checks the credentials by requesting a token. `NEXAR_CLIENT_ID`,
  `NEXAR_CLIENT_SECRET`, `NEXAR_COUNTRY`, `NEXAR_CURRENCY` override the file. Uses `supSearch` (keywords, 10
  parts) and `supSearchMpn` (MPN, 5 parts). One candidate per seller offer, SKU `<seller>:<seller SKU>`
  (`LCSC:C307331`), prices converted to the configured currency, unknown stock codes (negative
  `inventoryLevel`) as 0; brokers are never listed, unauthorized sellers only with `--unauthorized`. Specs give
  parameters (by attribute name), the package (`case_package`) and the lifecycle (`lifecyclestatus`).
  `similarParts` is not used as drop-in data (D26). Sources: token and endpoint
  <https://www.altium.com/documentation/altium-developer-center/octopart/api/authorization>, queries
  <https://www.altium.com/documentation/altium-developer-center/octopart/api/search> and
  <https://support.nexar.com/support/solutions/articles/101000494582>, field names from the GraphQL schema
  published by `https://api.nexar.com/graphql` (introspection).
- **LCSC / JLCPCB**: both have APIs (<https://www.lcsc.com/docs/index.html>, <https://api.jlcpcb.com>), but
  only for approved partners (LCSC: business license, IP whitelist; JLCPCB: application reviewed on order
  history), with endpoint documentation that is not public, and LCSC's terms forbid sharing technical aspects
  of the API with third parties. cadlab therefore ships no client (D33). Instead, `catalog.import` turns a
  parts list you download yourself (CSV) into an offline catalog with provider `lcsc`, the catalog the JLCPCB
  fab profile orders by, so `fab.check`, `fab.export` and the substitutes get LCSC `C` numbers. cadlab never
  scrapes websites.
- **PCBWay**: its partner API (<https://api-partner.pcbway.com/Help>) covers PCB and SMT quotes and orders, not
  parts search, stock or pricing; PCBWay sources assembly parts by MPN, which DigiKey, Mouser or Nexar answer.

Network providers sit behind the default `net` feature (`ureq`, rustls) and use a small transport trait
(`supplier::http`), so tests run them against recorded or hand-written JSON through a mock transport, never the
network. Credentials come from the user settings or the environment, never from projects, the cache or the MCP
path (D17); API keys sent in URLs are redacted from errors. Responses go through `supplier::cache` (user cache
dir, 24 h TTL; `CADLAB_OFFLINE=1` answers from the cache only). `cargo test --test suppliers_live` checks Mouser
and Nexar against the real APIs when their environment variables are set. Not done: Farnell (element14).

### Importing a parts list (`catalog.import`)

`catalog.import <file.csv> [--provider lcsc] [--currency USD] [--columns {...}] [--output F] [--replace]` writes
`<config dir>/catalogs/<provider>.json` (loaded automatically; `catalog.list` shows the providers). Excel files
must be saved as CSV (UTF-8) first. Neither JLCPCB nor LCSC publishes a stable export format, so columns are
recognized by header, ignoring case, spaces and punctuation:

| Field | Headers recognized |
|---|---|
| `sku` (required) | `LCSC Part #`, `LCSC Part Number`, `LCSC Part`, `JLCPCB Part #` (JLCPCB's BOM headers), `Supplier Part`, `SKU` |
| `mpn` (required) | `MFR.Part #`, `MFR.Part`, `Manufacturer Part Number`, `MPN` |
| `manufacturer`, `description`, `package` | `Manufacturer` / `MFR` / `Brand`, `Description`, `Package` / `Package / Case` / `Footprint` |
| `category` | `Category`, `First Category`, `Second Category`, `Subcategory` (all of them are used) |
| `stock`, `moq` | `Stock` / `Stock Qty` / `Inventory` / `Quantity Available`, `MOQ` / `Min Order Qty` |
| `price` | `Price` / `Unit Price`: one price (`0.0123`, `$0.0123`, `0,0123 €`) or breaks `1-199:0.0011,200-:0.0005` |
| `datasheet`, `url`, `lifecycle` | `Datasheet`, `URL` / `Product URL`, `Lifecycle` / `Status` |
| `class` | `Library Type` / `Part Type` (`Basic`, `Extended`), stored as the `part_class` parameter |
| parameters | headers named like distributor parameters (`Resistance`, `Capacitance`, `Tolerance`, `Voltage - Rated`, ...) |

Other headers are listed as `ignored`; `columns` maps them explicitly (`{"sku": "Code", "param:voltage_rating":
"Rated V"}`). For passives (resistors, capacitors, inductors, ferrite beads, fuses), values in the description
fill missing parameters (`10kΩ ±1% 62.5mW` → resistance, tolerance, power rating; `16V 100nF X7R` → voltage
rating, capacitance, dielectric); nothing is inferred for other categories. Rows without a SKU or MPN, and
repeated SKUs, are skipped with a `catalog.row_skipped` warning. An existing catalog needs `replace`
(`catalog.exists`); dry runs write nothing.

**Commands:**

| Command | Purpose |
|---|---|
| `part.search <keywords>` | filters: `category`, `package`, `params` (`{"current_out": ">=500mA"}`, `>= <= > <`, ranges match by containment), `in_stock`, `quantity`, `max_price`, `include_obsolete` |
| `part.create ... --fill-from-suppliers` | fills manufacturer, description, parameters, package and datasheet from the best listing of the MPN; pins still come from the datasheet |
| `bom.resolve [--boards N] [--apply]` | candidates for generic lines: same category, package and value, tolerance at most and ratings at least the requirement, enough stock; `apply` approves the best |
| `bom.check [--boards N]` | per line: ok, low stock, end of life, not found, no MPN; problems are error diagnostics (CLI exit code 3) |
| `bom.cost [--boards N]` | cheapest in-stock offer per line (MOQ and price breaks applied), totals per currency |
| `catalog.import <csv>` / `catalog.list` | import a downloaded parts list as an offline catalog (above); list the configured providers |
| `bom.substitutes [fab] [--boards N] [--part P] [--candidates K]` | ranked substitute candidates for lines that are not found, short of stock, end of life or without an MPN (with `fab`: at its catalog providers, with its lock's substitutions applied); see "Substitutes" |

Offers are never written into the project (D12). The library never judges or summarizes datasheets: it exposes
URLs and lets the agent read them.

## Substitutes (`src/substitute.rs`, DECISIONS D26)

When a line cannot be ordered as designed (its MPNs are not found at the providers asked, are short of stock
or end of life, or it has no MPN), `fab.check`, `fab.export` and `bom.substitutes` propose substitutes. For a
fab, "the providers asked" are its `catalog` providers when configured (LCSC for JLCPCB), so a part stocked by
DigiKey but not by LCSC needs a substitute at JLCPCB. The line's approved alternates are always tried first;
they are part of the line, so a substitute is only proposed when none of them works.

Candidates come from two sources, and nowhere else:

| Basis | Categories | Rule |
|---|---|---|
| `drop_in` | any | an MPN listed in a provider's cross-reference data (`Candidate::drop_in`) for one of the line's MPNs (looked up at every provider), offered by the providers asked, same package (normalized) and category when both are known |
| `parametric` | resistor, capacitor, inductor, ferrite bead, LED, fuse | same category and package, same value and other parameters (`dielectric`, color, ...), tolerance at most and voltage/current/power ratings at least the part's: the `bom.resolve` query; parts without package or value parameters get none |

ICs, regulators, transistors, diodes, crystals, connectors and anything else get drop-ins only. cadlab never
infers pin compatibility from MPN prefixes, descriptions or parameters; when no provider supplies
cross-reference data, the line gets no candidate and a note saying so (approve a replacement chosen from its
datasheet with `bom.approve`).

Every candidate is in stock for the build quantity and not obsolete or last-time-buy. Ranking is deterministic:
drop-ins before parametric matches, then in stock, active, cheapest for the quantity (MOQ applied), most stock,
MPN, provider; duplicates (same provider and SKU) are dropped. Each candidate lists the criteria it meets
(`package 0402`, `voltage_rating >= 16V`, `drop-in for X`).

Substitutes are suggestions: nothing changes the project. To use one:

- for every fab, as a design decision: `bom.approve <part> <mpn>` (an approved alternate);
- for one fab only: `fab.substitute <fab> <part> [mpn]`, recorded in that fab's `fab-lock.json`
  ([MANUFACTURING.md](MANUFACTURING.md)) and used by its next check and export.
