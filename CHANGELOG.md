# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.3](https://github.com/KarpelesLab/cadlab/compare/v0.0.2...v0.0.3) - 2026-10-05

### Other

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
