# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.6](https://github.com/KarpelesLab/cadlab/compare/v0.0.5...v0.0.6) - 2026-11-18

### Other

- guide 0.0.20 (D116): the ways out of the second ring of an array of balls planned by guide and its dog-bones away from the array's middle (glasgow-revC3 5 to 7 open for 29 to 34), the tracks from pad to pad that a pour joins taken out by the fill of the routed board where no connection opens and no pad is left short of its spokes (pcb::needless_between, thermal::joints_of), a run of guide's for shorter ways once the others are over where they leave the time (RouteOptions::shorten). Measured with the crate from crates.io, four runs of each router at 60 s side by side in the same half hour, three threads each: the twelve boards 102 to 139 open for guide, 335 to 376 for the native router, the eleven without glasgow-revD1 18 to 23 and 85 to 102; ahead in every run on buspirate5-rev10, lumenpnp-mobo, cynthion and the two glasgows, level on five, overlapping on corne-cherry (0 to 2 against 0 to 4) and tinytapeout-demo (0 to 7 against 2 to 15), behind on none; track within a tenth of the native router's median on four boards (buspirate5-rev10 +5 % for +17 %, corne-cherry -8 %) and on nine counted outside the pads of its net, lumenpnp-mobo +12 % for +20 %, tinytapeout-demo +23 % as it was; corne-cherry's vias 8 % over for 22 %, tinytapeout-demo's 327 to 462 against 238 to 290; 64 to 75 pads short of two necks for 73 to 77; 620 open at 40 million nodes over three orders for 675; worse than 0.0.19: glasgow-revD1 84 to 120 open for 93 to 112, tinytapeout-demo's vias by the median, glasgow-revC3's copper with its connections, time on lumenpnp-mobo and nrfmicro; cynthion's eight diagnosed on its board and left, tinytapeout-demo's negotiation looked at round by round; route_bench tells the track outside the pads of its net and writes a result's nets and tracks; the default unchanged
- guide 0.0.20 (D116): the tracks between pads that a pour joins taken out by the fill and the reliefs, a run for shorter ways once the others are over where they leave the time (code; the decision and the measurements follow)
- guide 0.0.19 (D115): its negotiation for every net at twice the pitch on boards of more than two layers (glasgow-revD1 negotiated: 93 to 112 open for 118 to 135), every via that the fill of the routed board makes needless told from that fill, with none made for it (pcb::needless), and taken out once the runs are over, the heat pads named as pads that vias of their net may be in where no track leaves them (cynthion's three exposed pads: 8 open for 11 to 12), the fill at a run's end given only before its deadline. Measured with the crate from crates.io, four runs of each router at 60 s side by side in the same half hour, three threads each: the twelve boards 142 to 157 open for guide, 342 to 475 for the native router, the eleven without glasgow-revD1 42 to 49 and 81 to 105; ahead in every run on buspirate5-rev10, lumenpnp-mobo, cynthion and glasgow-revD1, level on five, overlapping on corne-cherry (0 to 2 against 0 to 4), tinytapeout-demo (0 to 7 against 2 to 15) and glasgow-revC3 (29 to 34 against 31 to 36), behind on none; vias within 15 % of the native router's median on lumenpnp-mobo (336 against 339.5), not on tinytapeout-demo (369 against 261) nor corne-cherry (566.5 against 464.5), and under it on cynthion and glasgow-revC3; 73 to 77 pads short of two necks for 154 to 170 after the native router; 675 open at 40 million nodes over three orders for 710; worse than 0.0.18: track on the boards negotiated at twice the pitch (12 to 23 % over the native router's), one order of tinytapeout-demo with 7 open for 4 at most; what cynthion and glasgow-revC3 leave open looked at connection by connection; route_bench tells the vias of the nets with zones apart and the pieces their pours are in; the default unchanged
- guide 0.0.18 (D114): cadlab's own fill of the zones given to guide while it routes (RunControl::refill, host_fills), by which it tells the pads cut off their pours; the stubs to a plane that a pour makes needless told by guide at the end of each run, kept on the board between the runs and taken out once they are over; the default unchanged
- guide 0.0.17 (D113): every net routed against every other at once on one grid before guide's passes, in the first run of a run with a budget (RouteOptions::whole, PathFinder as the native router's negotiation has it, on rayon's threads); the directions the fill falls back to given as spare spokes and rules.min_thermal_spokes as the number a pad keeps. Measured with the crate from crates.io, four runs of each router at 60 s side by side in the same half hour, three threads each: the twelve boards 173 to 191 open for guide, 324 to 340 for the native router, the eleven without glasgow-revD1 55 to 59 and 81 to 101; ahead in every run on buspirate5-rev10, lumenpnp-mobo, cynthion and glasgow-revD1, level on four, overlapping on nrfmicro (0 to 1 against 0), tinytapeout-demo (0 to 4 against 2 to 15) and glasgow-revC3, behind on corne-cherry (5 to 10 against 0 to 4, for 15 to 21 with 0.0.16: every connection it has open is a pad that cadlab's fill leaves cut off from its pour); 88 to 97 pads short of two necks for 154 to 162 after the native router; 715 open at 40 million nodes over three orders for 732; guide's curated boards 1150 of 1151 with 712 vias; a count of spokes by pad with a price, a wider halo for stubs, a weight on the bound and wider batches tried and not kept; glasgow-revD1's first pass timed call by call (31 of 33 s in the searches); the default unchanged
- ems 0.0.18 beside openEMS (D111): the drift with the step traced to the absorbing layers' convolution by a study in the step, the end of a run by what the ports' signals still add at the lowest frequency with its cost per case, every validation before and after with what moved the wrong way first, the solid line's Z0 diagnosed
- sim.sparams settle: as many steps again at most (ems's own cap of a half left a lossy line run to 50 dB 0.004 dB from its reference at 0.06 GHz, where one run of openEMS read 0.0035)
- ems 0.0.18 (D111): the absorbing layers' convolution second order in time, and a run goes on past its energy criterion while the ports' signals still add to the lowest frequency (sim.sparams settle, on by default); the comparison's default thread count at most four
- guide 0.0.16 (D110): no via beside a via of its net (none of 48 runs had copper taken out by the verification); the spokes of pads in pours given to guide as the fill makes them (Zones::spokes, per zone and layer where the connection is thermal and the pad is soldered) and their room priced for other nets' tracks (spoke_cost 4): 82 to 100 pads short of two necks on the twelve boards over four orders at 60 s for 138 and 128 with 0.0.15 on the fill of D112, 154 to 170 after the native router, 116 as the projects route them; what the board itself keeps apart told as such and left out of the later runs (Cause::Apart, sweep-v2.2's two nets); a net with zones has its run on the filled board whatever the first run said; measured with the crate from crates.io, four runs of each router: the twelve boards 196 to 205 open for guide, 323 to 386 for the native router, the eleven without glasgow-revD1 70 to 84 and 81 to 105; ahead in every run on lumenpnp-mobo, cynthion and glasgow-revD1, overlapping on four, behind on corne-cherry (15 to 21 against 0 to 4) and by its median on nrfmicro (0 to 2 against 0); fixed ways out for shut-in pads, more cleanup, dearer vias and a carried width tried and not kept; the default unchanged
- D112, KiCad's starved_thermal on the spokes in other directions
- the KiCad install tries the package download again for about a quarter of an hour (Launchpad answered 503 for the .deb three times within a minute)
- BOARD.md, what fab.thermal_size is held to
- D112, what CI said of the corpus and the goldens that moved
- the type of the known Gerber differences
- the Gerber cross-check allows the one difference that is meant (the features board's J1.1 gets its fourth spoke on a diagonal, KiCad drops it, D112); the oracle job runs every test binary, not up to the first that fails
- the pads cut off are listed once in its output; zone.pad without pads lists
- Thermal relief (D112): the IPC-2581 golden of the tiny board has the spoke the fill now places along an axis at a round pad whose diagonal spokes do not all survive
- Thermal relief (D112): a track of the repair takes the width the net's class asks when one that wide fits, else the minimum track width
- Thermal relief (D112): the fab profiles say what each fab publishes about thermal relief (Eurocircuits: 0.20 mm gap and segment width; the others nothing of their own), fab.thermal_size holds to it; IPC-2221 9.1.3 and IPC-2222 9.1.2 as references of the rule presets; BOARD.md "Thermal relief" (one placed pad, the repair, what a router must be told), DECISIONS D112 with the corpus before and after and every pad left, ROADMAP, MANUFACTURING, TESTING
- Thermal relief (D112): the KiCad import takes a footprint that differs from the library's only in how some pads join zones as that footprint placed with those pads' own settings (no variant), and a pad width equal to the fitted one as unset; the oracle of the spoke count has the case of spokes in other directions; the list of commands
- Thermal relief (D112): the router's report, the DRC summary and the fab check name the pads short of spokes or without relief and the command that repairs them; a thermal relief minimum of their own in the fab profiles (thermal_relief: spoke, gap, spoke count, or the record that the fab publishes none); the KiCad install of the oracle and corpus jobs retries until the PPA answers and checks the version (Launchpad's 503s left Ubuntu's KiCad 7, whose kicad-cli has no pcb drc); the flow of an agent end to end as a test
- DRC (D112): the hole clearance check indexes the pours once and measures a hole against the segments near it: every hole of another net sits at the hole clearance from a pour, and the distance to the whole outline was taken for each (drc run on glasgow-revD1: 38 s to 4.5 s, same findings)
- Thermal relief (D112): spokes that do not survive are placed in the relief's other directions (the fill, when neither pad nor zone sets an angle); a connection per placed pad, per layer if needed (PlacedFootprint::pads, zone.pad); zone.relieve takes each pad short of spokes alone: a narrower relief, a short track into the pour, solid on the one layer where nothing fits, and leaves no pad cut off; mounting holes with a ring of vias are not solder pads; the KiCad export writes a placed pad's settings and the fitted spoke width on the pad
- Thermal relief (D109): the fitted spoke width takes the pad's shorter side to the nearest micrometer first and measures it with products, sums and square roots only, so a turned pad's rounded corners and the platform's hypot do not move it
- D109, thermal relief: the audit, the rules and why they are warnings, the defaults changed, zone.relieve on the corpus (664 solid solder pads to none, 161 more short of spokes, 84 pad and layer pairs no longer in a pour), the routers measured (219 and 195 pads short of spokes for 143 with the projects' routing) and left as they are, KiCad's starved_thermal as oracle
- Thermal relief (D109): drc.run with fab targets does not repeat the thermal relief findings under their fab codes
- Thermal relief (D109): the KiCad import leaves a zone's spoke width unset when it is cadlab's default, as before (KiCad rewrites what cadlab exports, so the generator cannot tell the two apart); waiver syntax in the hints
- Thermal relief (D109): zone.relieve says which pads the pour no longer joins (cut, zone.relieve_cut); heat pads are told by a grid of paste windows or by size with no other pad as large beside them (an imported pin with one paste aperture and the two lugs of a connector are solder pads), mirrored pads measured unsigned; KiCad's min_resolved_spokes raises min_thermal_spokes and never lowers it; KiCad's starved_thermal as the oracle of the spoke count on one fill (tests/kicad_pcb_oracle.rs), the corpus allowances for the rules only cadlab has; route_bench counts the pads in pours (CADLAB_BENCH_THERMALS); BOARD.md "Thermal relief", ROADMAP, TESTING
- Thermal relief (D109): pads in pours are read from the filled copper (board::thermal: spokes and necks that reach each pad, share of its outline in copper, per layer), drc.run and fab.check warn of solder pads without relief (drc.solid_pad, drc.thermal_nominal), of relieved pads short of spokes (drc.starved_thermal, rule min_thermal_spokes, default 2), of relief sizes below the minimums (drc.thermal_size) and of spokes too narrow for the net's current (drc.current_spokes); zone.thermals lists them and zone.relieve brings a board to relieved pads; a zone that leaves the spoke width unset fits it to small pads; generated exposed pads and tabs are solid on purpose; zone.fill and board.import_kicad count the pads in pours
- ems 0.0.16 and 0.0.17 beside openEMS (D107): the bound with the cells' shares balanced and the cut cells in it, every validation before and after, the step and the end of the run separated, the mesh at the region's walls evaluated and not taken, memory, the tail extrapolation tried
- ems 0.0.17 (D107): the time step from the cells with their shares balanced and the cut cells among them
- guide 0.0.15 (D108): two nets that cross in a row of fine pitch have the crossing planned on another layer where the row's has no room (RouteOptions::crossings); more runs that move routes, in other orders, while there is time and something open; the stubs to a plane that cadlab's fill makes needless taken out where a run leaves the time; measured with the crate from crates.io, four runs of each router in one hour: the twelve boards 195 to 238 open for guide, 199 to 287 for the native router, the eleven without glasgow-revD1 72 to 87 and 81 to 87; nrfmicro 0 to 1 for 2 to 5 (the native router 0), lumenpnp-mobo 510 to 571 vias in three orders for 699 to 738; ahead on lumenpnp-mobo and cynthion, overlapping on five, behind on corne-cherry (15 to 23 against 0 to 4); one run of 48 had a via on a via taken out by the verification (buspirate5-rev10, seed 3, as with 0.0.13); the default unchanged
- guide 0.0.14 (D106): a last run takes the routes of the runs before back as routes that move (pcb::route_from), after the run on the fixed board and kept only if the board has less open; at 60 s over four orders the twelve boards 890 open for 923 with 0.0.13 (corne-cherry 87 for 107, buspirate5-rev10 1 for 9, tinytapeout-demo 33 for 37, cynthion 64 for 67), no board with more for it; four runs of each router in one hour: the twelve boards 215 to 244 for guide, 197 to 292 for the native router, the eleven without glasgow-revD1 82 to 104 and 82 to 92; ahead on lumenpnp-mobo and cynthion, overlapping on four, behind on nrfmicro and corne-cherry; guide's negotiations for places settle corne-cherry's QFNs with four minutes (9 to 11 open for 13 to 16) and are left off at 60 s; the default unchanged
- guide 0.0.13 (D106): a route that keeps what it reaches begins another tree from a terminal it leaves out, so sweep-v2.2, two halves that only the schematic joins, has the 3 open that the native router has, for 6 or 7; both routers measured over four runs in one hour and compared by ranges: at 60 s the twelve boards 218 to 259 open over four orders for 237 to 267 with 0.0.12, the eleven without glasgow-revD1 91 to 98 for 104 to 116, the native router's four runs 80 to 107; level on four boards, ahead on two, overlapping on four, behind on nrfmicro and corne-cherry; at 40 million nodes 801 for 814; ROUTER.md names the freerouting-corpus job for the corpus tables; the default unchanged
- D104's fourth gap found: sweep-v2.2 is two halves that only the schematic joins, and guide leaves the pads of the half without a net's first terminal on their own; the change that joins them is on guide's branch forest-wip, unreleased, as it costs tinytapeout-demo and buspirate5-rev10
- D104's fourth gap: what shuts sweep-v2.2's three nets in is not other nets' routes
- guide 0.0.12 (D104): each connection its passes leave open is negotiated for before its chains, the nets near it each giving way a little at a price that rises; the rows of fine pitch planned on no board again, which gives the generated stm32-2l its 72 vias back for 126; at 60 s over three orders 256, 235 and 217 open on the twelve boards for 267, 247 and 224 with 0.0.11 (the eleven without glasgow-revD1 94, 100 and 94 for 105, 111 and 101, the native router 93; corne-cherry 29, 28, 26 for 31, 36, 33; buspirate5-rev10 0, 2, 0 for 0, 6, 3; nrfmicro 2, 4, 3 for 3, 5, 3); the native router's own runs of glasgow-revD1 are 115 to 200 open; the default unchanged
- the time step from a bound over the cells, part by part; every validation against its reference before and after; ems 0.0.15 beside openEMS; what is left and why; the CI jobs divided (D105)
- ems against openEMS and freerouting on the corpus as jobs of their own, on the weekly schedule and by hand; the oracle tests stay on every push (D105)
- ems 0.0.15 (D105): the time step from its bound over the mesh's cells; long runs of the oracle's cases stay bounded
- ems beside openEMS, measured: the comparison's table before and after ems 0.0.13 and 0.0.14, the openEMS tables of ELECTRICAL.md read again on ems with D52 beside the lines, what openEMS is still ahead in, its result moving with the machine (D103)
- ems 0.0.14 (D103)
- openEMS oracle: a part against circuit theory between its lines as the field solver has them, beside the ideal (D103)
- ems 0.0.13 (D103)
- openEMS oracle: the via once more with the thirds rule stated for both solvers (the oracle's case runs two meshes), a finer mesh per case as a reference, the table written after every case (D103)
- openEMS oracle: the comparison's summary row per case, ems's time stepping apart from its setup, the time step, the lowest frequency on its own row (D103)
- openEMS oracle: ems and openEMS side by side per case (time, steps, cells, peak memory, each against D52 or circuit theory), one process per run on stated threads; the driver takes CADLAB_OPENEMS_THREADS; the job's openEMS cache under runner.temp (D103)
- freerouting 2.5.0 on the corpus beside the native router and guide: the tables at 60 s, 30 minutes and 2 hours, what a DSN does not carry, where freerouting is ahead; the M6 exit as measured; D102
- with freerouting, what of each board a DSN has no place for, counted; clearance errors with copper drawings classed as not carried; ROUTER.md's bench and oracle paragraphs name freerouting 2.5.0 and its Java (D102)
- freerouting on the corpus: DRC errors classed one by one against what the DSN carries, kept sessions counted again without a run; TESTING.md and the DSN limits (D102)
- freerouting on the corpus: route_bench hands the corpus boards to freerouting and counts its sessions with cadlab's DRC; export.dsn can write pours as planes; a CI job for the 60 s table; the corpus cache path without `..` (D102)
- guide 0.0.11 (D101): 0.0.10 without its chains for the pads cut off their pours, which kept lumenpnp-mobo's first run from coming back in time on a loaded machine; measured again on the machine as D97 had it: 263, 249 and 234 open on the twelve boards for 273, 249 and 243 with 0.0.9 and the native router's 290 (nrfmicro 3, 5, 3 for 4, 7, 9; corne-cherry 31, 37, 36 for 36, 35, 38); the generated boards complete, stm32-2l with 126 vias for 72 with its rows planned; the default unchanged
- guide 0.0.10 (D101): its passes of rip-up end where they have stopped gaining and what they leave open is settled by chains kept only if the board has less open; `settle` on, `row_fanout` on boards of two copper layers; measured with a budget of work (853 open for 892 on nine boards over three orders, nrfmicro 11 for 20 and corne-cherry 130 for 169 with the rows) and at 60 s on a loaded machine (412, 408, 343 for 419, 468, 362; native 370); lumenpnp-mobo loses time to the chains for pours, recorded; the default unchanged

## [0.0.5](https://github.com/KarpelesLab/cadlab/compare/v0.0.4...v0.0.5) - 2026-11-17

### Other

- D100's CI run: eleven jobs pass on ci/ems-always, the openEMS oracle against sim.sparams on ems alone (D100)
- sim.sparams on ems alone: the full-wave section describes one solver, its model and its accuracy against analytic references, with the removed engine's comparison as a record; what is lost (the second result on the same model, a build without the solver, its lower memory); ROADMAP's last stretch item ticked (D100)
- sim.sparams runs on ems alone: the cargo feature `ems` and the built-in FDTD engine are removed, `solver` is no longer a parameter and the result names `ems 0.0.12`; the tests keep their analytic references, and the engine's own results stay as recorded figures (D100)
- guide 0.0.9 through the adapter at 60 s over three orders of the nets as the last row, 0.0.8 behind the same adapter beside it, the via on a via found in guide and gone, the generated boards again (D97)
- guide 0.0.9 (D97)
- guide 0.0.8 through the adapter at 60 s over three orders of the nets, 0.0.7, 0.0.8 behind the same adapter and with the longer first run beside it and the native router, the share measured again, the nets with zones in the first run, what was tried and not kept, what is left for guide, the default unchanged (D97)
- guide 0.0.8 in the adapter: the first run has 0.7 of the budget and the nets with zones among its nets, open or not; pour_cuts on and row_fanout off by name (D97)
- guide 0.0.8 (D97)
- ems 0.0.12 measured on every case of D90's table against the built-in solver: memory 1.3 to 1.8 times its own, a Debye laminate on one thread 1.1 times, session 6 at equal threads 0.7 to 0.9 times; the openEMS oracle with ems as the default; what removing the built-in engine waits on (D98)
- sim.sparams on ems: a port's run gives its simulation up for its results (D98)
- ems 0.0.12 (D98)
- exchange oracles job: FreeCAD 1.1.4 reads the STEP files, xmllint validates IDX against prostep ivip's schema; listed oracles fail instead of skipping; the IPC-2581 schema is refused by IPC's server and stays open (D99)
- guide 0.0.7 through the adapter at 60 s over three orders of the nets, 0.0.6 and 0.0.7 before the rules beside it and the native router, the custom rules as areas and pads' own clearances, the share and the second run measured again, what is left for guide, the default unchanged (D96)
- guide 0.0.7 in the adapter: a courtyard's or an area's clearance rule goes over when its kinds take in tracks and vias, raised to the later rules that hold copper in it; the last rule holding a pad is the pad's own clearance (D96)
- guide 0.0.7 (D96)
- guide 0.0.6 through the adapter at 60 s, before and after with vias and length beside the native router, the share, the second run and join measured again, lumenpnp-mobo looked into, what is left for guide, the default unchanged (D95)
- guide 0.0.6 in the adapter: after a cut run the second run takes every open net, Routed::failed leaves nets out only after a run that ended, the first run has 0.45 of the budget (D95)
- guide 0.0.6 (D95)
- the work count that differed between 2 and 4 threads on glasgow-revC3 comes from polyclip 0.0.6's sweep status, not the router
- a pour round taken back gives every island the fills before it again
- ems 0.0.11 measured on every case of D90's table, criterion 3 met, sim.sparams on ems by default; what is open: memory, one thread, session 6 at equal threads (D94)
- session 6's full-wave run on both solvers, each by name (D94)
- ems 0.0.11 (D94)
- ems is the default solver where the build has it, solver: builtin kept; the result names its solver (D94)
- measure_memory on the rest of D90's table (the sheet line, the small via region, the bent line with solid copper and on the default mesh), its Touchstone file kept; session 6 on a given thread count (D94)
- guide 0.0.5 through the adapter at 60 s, before and after, the half and the second run measured again, what is left for guide, the default unchanged (D93)
- guide 0.0.5 in the adapter: no joining of zones' pieces, a stopped connection says whether its net had its turn, no wording of its own for "found no way" after a deadline (D93)
- guide 0.0.5 (D93)
- a via in a pad is one in the pad's copper, not in its box (D93)
- ems 0.0.10 measured against the built-in solver, D89's three findings gone, the one case of run time left and why the default stays (D90)
- the 60 mm line holds ems to the built-in solver's 6 % again; measure_memory, one run per process (D90)
- sim.sparams --solver ems: ems 0.0.10; a laminate's poles averaged at its surface, a negative energy is ems's to end on (D90)
- guide 0.0.4 through the adapter at 60 s, before and after, what got in the way, the default unchanged (D91)
- guide's "found no way" after a run its deadline stopped says the pad was not looked at (D91)
- guide 0.0.4 in the adapter: no fill at the end, the smallest via for escapes, half the time budget for a second run on the routed board (D91)
- the smallest via the rules allow is one function, for the fanout and for guide's escapes (D91)
- guide 0.0.4 (D91)
- ems 0.0.9 measured against the built-in solver, the criteria for the default and why it stays (D89)
- the built-in solver's board-level tests on both solvers; the openEMS oracle compares each (D89)
- sim.sparams --solver ems: ems 0.0.9; small faces, narrow tracks and vias of a cell are ems's own, the adapter's workarounds go (D89)
- guide 0.0.3 as a backend, both routers measured again, what still blocks it (D88)
- guide under the same budgets, with a seed when one is given (D88)
- guide 0.0.3 in the adapter: budgets, causes and the host's board model in place of workarounds (D88)
- ems 0.0.7 as the second solver, the two solvers measured again, the default as measured (D87)
- sim.sparams --solver ems: ems 0.0.7's lossy copper, conformal outlines, typed warnings and one compile (D87)
- guide as a backend, both routers on the generated boards and the corpus, the default as measured (D85)
- the backend choice, the seed, DRC errors by code, vias in pads (D85)
- guide as a second backend behind router: guide, verified and counted as the native router (D85)
- the exact outline of a pad that shares its number and position with another (D85)
- ems against the built-in solver on its validation cases, measured; what ems needs before it replaces it (D86)
- the ems crate as a second solver on the built-in solver's region, sim.sparams --solver ems (D86)

## [0.0.4](https://github.com/KarpelesLab/cadlab/compare/v0.0.3...v0.0.4) - 2026-11-13

### Other

- Silkscreen DRC: drawings against drawings, texts drawn where KiCad draws them, corpus compared without allowances (D83)
- corpus re-route after D84, the residuals diagnosed, the M5 exit as measured (D84)
- grid lines along the finest rows, exits off the grid lines, finer grids after a run that leaves a few connections (D84)
- list the copper around a component, plan the fanout alone
- edge and overlap DRC rules, reference designators arranged and kept in the board (D82)
- corpus re-route with pours, the router's exit criteria as measured (D80)
- pours are places to connect to, re-planned where the routing cuts them, failures counted on the refilled board (D80)
- list the connections a corpus project's own routing leaves open
- the demo board's KiCad DRC oracle runs in the oracles job
- fillet tables compared with published IPC-7351B reproductions, BGA lands from the land table (D78)
- Milestone exits: M1 and M3 shown by tests, M4 demo board with both fab bundles, exit lines for M7-M9 (D81)
- net exceptions, honored by DRC, zone fill, router and route.track; reported by DSN, IDF, IDX and KiCad exports (D76)
- IPC rule presets: values confirmed on secondary sources and moved to a data file; IPC-2152 fit is the universal chart (D79)
- Clippy 1.100: needless_range_loop and needless_bool
- Fab profiles: re-checked against the fabs' pages, unverified values closed where published (D77)

## [0.0.3](https://github.com/KarpelesLab/cadlab/compare/v0.0.2...v0.0.3) - 2026-11-04

### Other

- Fab zips through fstool; MSRV 1.95 (D75)
- absorbing cells beyond the region, via barrels as node columns, board parts as lumped elements (D74)
- conformal copper on diagonal and curved edges, mesh refined for narrow diagonal tracks (D73)
- work budget for repeatable comparisons and users, verification inside the budget, via-aware estimate above two layers (D72)
- Session 5 leftovers: part.set pins, describe views, inherited regions, flow-through orientation, connectors along edges, honest board use (D71)
- Agent session 6: 2.4 GHz node through MCP: sim.sparams --plan, part values per run, results at a frequency with impedance and checks, mesh slivers (D70)
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
