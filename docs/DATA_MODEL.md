# Data model

## Units

| Quantity | Type | Representation |
|---|---|---|
| Length / coordinate | `Nm` | `i64` nanometers. ±9.2 × 10⁹ m range, 1 nm resolution. KiCad uses i32 nm; we keep headroom for intermediate math. |
| Angle | `Angle` | `i32` in millidegrees (0.001°). Multiples of 90° get exact fast paths. |
| Electrical values | `Value<Unit>` | decimal mantissa + SI exponent (`10k`, `100n`, `4.7u`), never `f64`, so `4.7uF` stays `4.7uF` |

Parsing accepts human input everywhere: `"0.2mm"`, `"8mil"`, `"0.1in"`, `"10k"`, `"100nF"`. Output uses mm by
default, configurable per project. Bare numbers without units are rejected in the public API to avoid mm/mil
confusion (an easy mistake for agents).

Board coordinates: X right, Y **up** (math convention, like Gerber), origin at a project-defined point (default:
lower-left of the board outline). KiCad import/export converts its Y-down convention.

## Identifiers

Two layers:

1. **Internal IDs**: opaque, stable, unique (`ObjectId`, 64-bit, allocated per project). They never change, even
   when something is renamed. Used for references inside the model and for undo.
2. **Names**: what humans and agents use. Every command accepts names and resolves them:

| Object | Name syntax | Example |
|---|---|---|
| Component | refdes | `U1`, `R12` |
| Pin | `refdes.pin` (number or name) | `U1.4`, `U1.PA9`, `J1.VBUS` |
| Net | `net:` prefix optional where unambiguous | `VBUS`, `net:/usb/D+` |
| Part | library path or MPN | `local:ldo_3v3`, `mpn:AP2112K-3.3TRG1` |
| Layer | canonical name | `F.Cu`, `In1.Cu`, `B.Cu`, `F.SilkS` |
| Board items | type + index or ID | `via#42`, `track#1203`, `zone:GND_bottom` |

Resolution errors list the closest matches.

## Project structure

```
Project
├── manifest        name, version, schema version, units, optional compatibility targets, metadata
├── library         parts available to this project (local copies, pinned, see PARTS.md)
├── bom             sourcing overlay: requirements, approved MPNs + alternates, DNP, cached supplier data
├── circuit         SOURCE OF TRUTH for connectivity
│   ├── components  refdes → part, properties, block membership
│   ├── nets        name → set of pins, net class
│   ├── blocks      reusable hierarchical subcircuits and their instances
│   └── netclasses  rule sets referenced by nets
├── schematic       OPTIONAL presentation: symbol positions/hints; regenerated when absent
├── board
│   ├── stackup     layers, thicknesses, materials, copper weights, board spec (finish/color preferences)
│   ├── outline     edge cuts, cutouts, slots
│   ├── rules       design rules (engineering intent, fab-independent), per net class overrides
│   ├── footprints  component placement: position, rotation, side, locked
│   ├── tracks      segments and arcs: layer, width, net
│   ├── vias        position, drill, pad, layer span (through/blind/buried/micro)
│   ├── zones       polygon, layer(s), net, priority, fill settings (+ cached fill)
│   ├── keepouts    rule areas
│   └── graphics    silkscreen text/lines, fab notes, logos
└── outputs         output job definitions (which files, which formats); fab choice happens at export
```

### Why netlist-first

KiCad's source of truth is the schematic drawing: connectivity comes from wires touching pins on a canvas. That
works for humans with a mouse and is awkward for programs: an agent would have to compute coordinates just to say
"connect U1 pin 4 to VBUS".

In cadlab, connectivity is data: `connect U1.4 VBUS`. The schematic is a generated drawing for review, like a
rendered report. Hints (keep these together, put this on the left) can be stored to improve it, but they never
affect connectivity.

### Circuit editing

Connectivity is edited with commands, never by drawing:

```sh
cadlab net connect VBUS J1.VBUS U1.VIN C1.1      # pins by number or name; U1.GND = every GND pin
cadlab net connect "DATA[0..7]" U1.PA0..PA7 J2.1..8   # bus: ranges spread over DATA0..DATA7
cadlab net set VBUS --driven                      # powered from a connector (ERC)
cadlab net no-connect U2.PB4                      # intentionally open
cadlab block create status_led R2 D1               # capture; then: block instantiate status_led LED2
cadlab net set VBUS --voltage 5V --current 1.5A   # electrical properties (SPICE, IPC-2152 DRC check)
cadlab circuit erc                                # exit code 3 on errors
cadlab circuit lint                               # design lint: decoupling, I²C pull-ups, USB ESD, ...
cadlab circuit export out/board.net               # KiCad netlist; --format json
cadlab circuit import kicad.net                   # KiCad netlist in (kicad-cli sch export netlist); --replace
```

`circuit.json` stores nets as sets of `REFDES.PIN` (pin *numbers*), one net per line. Connecting a pin that
is already on another net merges the nets only with `merge: true`.

### Circuit ↔ board consistency

The board references components and nets from the circuit. When the circuit changes:

- New components appear as unplaced footprints.
- Removed components get flagged. Their footprints and attached copper are removed on the next `board sync`, or
  immediately with `--prune`.
- Net changes are reflected immediately in the ratsnest. Copper that now connects different nets shows up in DRC.

There is no separate "update PCB from schematic" step to forget. Sync is automatic and diagnostics report what changed.

### Provider independence

Nothing in the project names a fab or a supplier as a requirement: rules, board spec and BOM express intent, and
fab-specific choices are made at export (see [MANUFACTURING.md](MANUFACTURING.md#provider-agnostic-projects)).

## On-disk format

A project is a directory, designed for git:

```
myboard/
├── cadlab.toml            # manifest (TOML: hand-editable)
├── library/
│   └── <part-id>.json     # one file per part
├── bom.json
├── circuit.json
├── schematic.json         # optional
├── board.json
├── outputs.toml
├── .cadlab/               # cache: supplier responses, zone fills, session/undo state (gitignored)
└── out/                   # generated outputs, e.g. out/jlcpcb/ with its fab-lock.json (gitignored by default)
```

Rules for the JSON files:

- **Deterministic:** keys and collections in a canonical order, so saving twice gives identical bytes.
- **Diff-friendly:** a custom pretty-printer puts each leaf geometry record on one line, so moving one via
  changes one line:
  ```json
  "vias": [
    {"id": 812, "at": ["12.7mm", "8.4mm"], "drill": "0.3mm", "pad": "0.6mm", "net": "GND"},
    {"id": 813, "at": ["14.2mm", "8.4mm"], "drill": "0.3mm", "pad": "0.6mm", "net": "GND"}
  ]
  ```
- **Schema-versioned:** `schema_version` in the manifest. Loading an older version runs migrations. A published
  JSON Schema per file lets editors and agents validate files.
- **Units in files:** stored as strings with explicit units (`"12.7mm"`), parsed exactly to `Nm`. Readable,
  unambiguous, and lossless as long as values are on the nm grid.
- **Derived data is not stored** (ratsnest, connectivity graph, DRC results), except zone fills, which are
  expensive and cached in `.cadlab/`.

Single-file export (`cadlab project pack`) bundles everything into one `.cadlab.json` for easy transfer.
