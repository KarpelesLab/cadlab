# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.3](https://github.com/KarpelesLab/cadlab/compare/v0.0.2...v0.0.3) - 2026-10-22

### Other

- USB-C crossovers leave the outer pads an exit, time for the boxed-pad re-plan, partial rip-up of large nets (D69)
- rustls-webpki 0.103.16, webpki-roots 1.0.10: clear RUSTSEC-2026-0381 and RUSTSEC-2026-0382
- openEMS export at parity with sim.sparams, openEMS oracle CI job, CPML shift at f0 (D68)
- Spread placement: crowding over the density, jumps to where connections pull, ratsnest budget per move (D67)
- Agent session 5: mixed-signal logger with blocks through MCP (D66)
- board parts as lumped series R-L-C elements from their part values (D65)
- zip 9.0: clear RUSTSEC-2026-0367 and two other extraction advisories
- 3D models: STEP and VRML through oxideav-step and oxideav-vrml (D36)
- Fix master CI: fab paths with / on every OS; KiCad export timing baseline includes its zone fill
- escapes planned per package, boxed pads re-planned; negotiation estimates costs near targets (D64)
- FDTD losses: surface impedance on conductor loops, Djordjevic–Sarkar dielectrics, solder mask (D63)
- Session 4 follow-up (D62): KiCad DRC exclusions as waivers, keep-together hints, place.near over routing, route.nets shoving leftovers, KiCad zone fills, supplier part numbers from fields
- scoped custom rules, local and hole clearances; flow-through pair parts in series (D61)
- Pour residuals (D60): copper rings around non-plated holes, spokes along rows of same-net pads, open connections paired by island
- Built-in 3D FDTD solver: sim.sparams on the openEMS export's region and mesh (D59)
- Agent session 4 through MCP: KiCad migration, hints with JSON args, DRC filters and waivers (D58)
- BGA fanout routing gets a budget slow CI runners cannot exhaust
- corpus completion pass (D57)
- Corpus residuals (D56): exact clearance gaps, KiCad-measured tolerances, courtyard pieces and sides, copper-to-hole clearance, pad stacks in every output
- Update rustls to 0.23.46 (RUSTSEC-2026-0346, RUSTSEC-2026-0347 and the brotli advisory fail cargo-deny)
- Signal-path lengths with stubs, spread placement over the board, fab stackup listing (D55)
- openEMS export assumes the same loss tangent as the loss calculations
- board region to openEMS (CSXCAD geometry, graded mesh, Python driver); Touchstone reader and touchstone.summary (D54)
- Agent session 3: offset bodies, placement hints, fab stackups, buck-aware ERC/lint (D53)
- frequency-dependent losses (loss tangent, skin effect, roughness), sweeps and net.loss (D52)
- Corpus re-measured with KiCad 10.0.7 after D44-D48; reference fill trims spokes; docs (D51)
- KiCad import: map KiCad 6/7 pad tstamps in the UUID table
- Zone fills: trim thermal spokes at other items' clearances instead of dropping them
- minimum to pad copper, overlaps only for mask openings; map pad-less footprint drawings in the import UUID table
- KiCad 10.0.7 drops a clearance next to a crossing; probe silk clearance near a pad
- Agent session 2: spread auto-placement, unrouted DRC summary, nested enums in describe, KiCad footprint names, footprint.create (D50)
- Impedance field solver: 2D boundary element method for lines and pairs (D49)
- Per-layer pad stacks: modeled, imported from KiCad 9 padstack, exported, in every consumer (D48)
- MCAD exchange: generated STEP bodies, IDF outlines/keep-outs, IDX change and response messages (D47)
- DRC/pour parity with KiCad: silk vs mask openings, thermal spoke angle/count, rule pairs, disallow, severities (D46)
- Agent session 1: CLI shortcuts and argument forms, ERC drivers, silk margins, hints (D45)
- Trapezoid and chamfered pads, drill offsets: modeled, imported, exported, in every output (D44)
- router regression and large-board timings checked against committed baselines (D43)
- 2D and 3D field solvers as stretch goals
- Schematic layout: 7.3 s → 0.3 s on the synthetic board, sheets identical
- polyclip 0.0.6: zone fill ~6× faster, outputs byte-identical
- netlist golden file no longer depends on the crate version

## [0.0.2](https://github.com/KarpelesLab/cadlab/compare/v0.0.1...v0.0.2) - 2026-10-04

### Other

- gridless search over expansion rooms (`router: grid|gridless|auto`), neck-downs, small dog-bone vias (D39)
- plain check instead of a one-element loop (clippy 1.99)
- Corpus manifest and docs follow the modeled import gaps (D40)
- Tests for local settings, net ties, slots and custom rules; crosscheck board
- model local pad settings, net ties, back pads, copper drawings, slots, custom rules (D40)
- D42, spatial index done, performance tables, polyclip wishlist
- Corpus timings in the ignored perf test; polyclip reproduction example
- one-pass Prim and cached net costs in the improvement loop
- skip primitives and region rings that leave no pixel
- index the board outline's segments for edge and containment checks
- Zone fill cache compares its inputs instead of serializing them
- Packed R-tree for shape boxes; zone fill culls obstacles per zone
- polyclip 0.0.4
- Bench harness: corpus boards as optional input, output dumps for before/after comparisons
- run the KiCad library import oracle in the oracle job
- KiCad library import (PARTS, DATA_MODEL, TESTING, D41, roadmap)
- import the user's KiCad footprint and symbol libraries (D41)
- pin units and alternate functions, footprint provenance
- release v0.0.1 ([#1](https://github.com/KarpelesLab/cadlab/pull/1))

## [0.0.1](https://github.com/KarpelesLab/cadlab/releases/tag/v0.0.1) - 2026-10-04

### Other

- release-plz (release PRs, crates.io publish) and release binaries
- Merge branch 'worktree-agent-a30db24ea8691167b'
- open-source KiCad corpus fetched in CI, with the fixes it found
- Merge branch 'worktree-agent-af2b36841cf02a34d'
- Router M6 phase 1: benchmark suite, BGA/fine-pitch fanout, parallel batches, shove
- build the default path from components (Windows separators)
- Merge branch 'worktree-agent-afd5f6666b82caf86'
- Specctra DSN export and SES import (export.dsn, route.import_ses)
- Merge branch 'worktree-agent-a10e5abfe6c022344'
- Schematic layout: block frames, collision-checked attachments, skyline packing, multi-sheet
- Merge branch 'worktree-agent-a7b8ec557a1514cb5'
- M4 outputs: Gerber X2/X3, XNC drill, pick-and-place, IPC-D-356A
- M4 foundation: board model, placement, manual routing, ratsnest
- M3 (part 1): renderer, schematic auto-layout, render commands
- M2 (part 2): reusable blocks; M2 complete
- M2 (part 1): nets, net classes, ERC, netlist export
- live-tested search fixes, package aliases
- User settings file and `cadlab config` for DigiKey credentials
- Use polyclip 0.0.2
- Format tests
- DigiKey provider (Product Information API v4)
- M1 (part 2): part research, BOM resolution, stock checks and costing
- M1 (part 1): parts, footprints, symbols, components, BOM
- Use polyclip 0.0.1 from crates.io
- LF line endings for golden files, version polyclip git dependency
- foundations, roadmap and design docs
