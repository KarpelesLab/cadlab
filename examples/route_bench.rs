//! Router benchmark suite (`tests/common/routebench.rs`, docs/ROUTER.md "Benchmarks"):
//! `cargo run --release --example route_bench [names...]`.
//!
//! Prints a Markdown table: completion, vias, wirelength, segments, sharp corners, runtime and
//! DRC errors per board. With `CADLAB_ORACLE_FREEROUTING=/path/freerouting.jar` every board is
//! also routed by freerouting (external process, DSN/SES) for comparison. `CADLAB_ROUTE_RENDER=<dir>`
//! renders cadlab's results; `CADLAB_BENCH_BUDGET_MS` sets the time budget (default 120000);
//! `CADLAB_BENCH_ROUTER=grid|gridless|auto` picks the search (default: the router's default) and
//! `CADLAB_BENCH_ROUTER=guide` the guide crate's router in its place (D85, D88, D91, D93, D95, D96, D97, D101, D104, D106, D108, D110, D113, D114, D115, D116, D117; here and in the
//! corpus mode), under the same time budget or work budget (`CADLAB_BENCH_WORK`, in guide's
//! search nodes there). `CADLAB_BENCH_VIA_IN_PAD=1` lets guide place vias in pads, as it does on
//! its own; without it the adapter forbids them, as the native router does. With
//! `CADLAB_BENCH_VERBOSE=1` both modes count the vias in surface-mount pads of their net, and the
//! corpus mode tells the vias and the track of the nets with zones apart from the others', by net,
//! and the pieces that the pads and fills of each such net are in without the net's new copper
//! (what a router had to join, by a via to a plane or by a track; D115), and the length of track
//! that is outside the pads of its net: a router that takes a track to the middle of its pad and
//! one that ends it at the first point on the pad's copper differ by what is inside, which is
//! copper on copper (D116). `CADLAB_BENCH_BY_NET=<dir>` then also writes, per board, `<board>.nets`
//! (by net: length in mm, vias, whether it has a zone, whether it is left open) and
//! `<board>.tracks` (every new track and via and every pad, in nanometres), for two routers'
//! results side by side.
//! `CADLAB_BENCH_SEED=<n>` sets the seed (default 1; guide's default is none, its nets shortest
//! first).
//!
//! **Corpus re-route mode**: `route_bench --corpus [names...]` imports the boards of the
//! open-source corpus (`tests/corpus/projects.toml`, fetched by `scripts/fetch-corpus.sh` into
//! `CADLAB_CORPUS_DIR`, default `target/corpus`; circuits built from the boards, no KiCad needed),
//! rips every track and via (zones stay), routes again and prints completion, vias, length, time,
//! work and DRC errors per board, with the failed connections grouped by reason
//! (`CADLAB_BENCH_VERBOSE=1` lists each one, and each new DRC error). The budget defaults to 60 s here;
//! `CADLAB_BENCH_WORK=<units>` replaces it by a work budget, which gives the same result on any machine,
//! under any load and with any number of threads, so two builds can be compared exactly (with
//! `CADLAB_BENCH_SEED=<n>` for the seed, default 1: negotiation is chaotic, so compare several).
//! `CADLAB_BENCH_ROUTER=freerouting` hands each board to freerouting instead (an external process
//! on the DSN `export.dsn` writes, `CADLAB_ORACLE_FREEROUTING=/path/freerouting.jar`, run with
//! `CADLAB_ORACLE_JAVA`, else `$JAVA_HOME/bin/java`, else `java`), applies its session as
//! `route.import_ses` does and counts the result the same way: the open connections are those
//! cadlab's DRC finds with the pours filled again, the DRC errors are split into those of rules
//! the DSN carries and those it has no place for. The budget is freerouting's own time limit
//! (`--router.job_timeout`; `CADLAB_BENCH_FR_TIMEOUT=none` sets none, and freerouting's own is
//! 12 hours), `CADLAB_BENCH_FR_THREADS` the processors its JVM sees and its `-mt` (default 1),
//! `CADLAB_BENCH_FR_PASSES` its `-mp`
//! (default: its own), `CADLAB_BENCH_FR_PLANES=1` writes the pours as planes
//! (`export.dsn {planes: true}`), `CADLAB_BENCH_FR_ARGS` adds arguments and `CADLAB_BENCH_KEEP=<dir>`
//! keeps the DSN, the session, freerouting's log and its result manifest per board
//! (`CADLAB_BENCH_FR_REUSE=1` then counts the kept sessions again without running it). The run fails
//! when a board has no result (docs/ROUTER.md, "freerouting on the corpus"; D102).
//! `CADLAB_BENCH_TRACE=1` prints a timeline of the router's phases and negotiation iterations;
//! `CADLAB_BENCH_THERMALS=1` adds, on stderr, the pads in pours of each board as it stands
//! (`board::thermal`: by the necks that reach them, solid pads by kind, the thermal relief
//! findings by code; with `CADLAB_BENCH_ORIGINAL=1` of the project's own routing), and
//! `CADLAB_BENCH_THERMALS=relieve` the same after `zone.relieve`'s changes, with their time.
//! `CADLAB_BENCH_NETS=a,b` routes only those nets. `CADLAB_BENCH_ORIGINAL=1` lists the connections
//! the projects' own routing leaves open instead (the reference for residual failures).
//! For looking at one place: `CADLAB_BENCH_DUMP=<refdes>[.<pad>][,<margin mm>]` lists the pads,
//! tracks and vias of the result around a component or pad, and `CADLAB_BENCH_FANOUT=only` plans
//! the fanout alone (dog bones, crossovers, escapes) and lists that.

#[path = "../tests/common/routebench.rs"]
mod routebench;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cadlab::kicad_import::{self, BoardImportOptions, OriginMode, rules};
use cadlab::model::Project;
use cadlab::router::{self, Hooks, RouteOptions, Scope, SearchKind, WorkMeter};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--corpus") {
        args.remove(0);
        corpus(&args);
        return;
    }
    let filters = args;
    let budget: u64 = std::env::var("CADLAB_BENCH_BUDGET_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(120_000);
    let render = std::env::var_os("CADLAB_ROUTE_RENDER").map(std::path::PathBuf::from);
    let fr = routebench::freerouting_tool();
    println!("{}", routebench::HEADER);
    for case in
        routebench::cases().iter().filter(|c| filters.is_empty() || filters.iter().any(|f| c.name.contains(f.as_str())))
    {
        let m = routebench::run_cadlab(case, budget, render.as_deref());
        println!("{}", routebench::row(case, &m));
        if let Some(tool) = fr.as_ref().filter(|_| case.freerouting) {
            let m = routebench::run_freerouting(case, tool);
            println!("{}", routebench::row(case, &m));
        }
    }
}

#[derive(serde::Deserialize)]
struct Manifest {
    project: Vec<Entry>,
}

#[derive(serde::Deserialize)]
struct Entry {
    name: String,
    pcb: String,
    layers: u8,
}

/// The search picked by `CADLAB_BENCH_ROUTER`.
fn search_kind() -> Option<SearchKind> {
    match std::env::var("CADLAB_BENCH_ROUTER").ok()?.as_str() {
        "grid" => Some(SearchKind::Grid),
        "gridless" => Some(SearchKind::Gridless),
        "auto" => Some(SearchKind::Auto),
        "guide" => Some(SearchKind::Guide),
        // Not a search of cadlab's: the corpus mode hands the board to freerouting instead.
        "freerouting" => None,
        other => panic!("CADLAB_BENCH_ROUTER: unknown router `{other}` (grid, gridless, auto, guide, freerouting)"),
    }
}

/// `CADLAB_BENCH_THERMALS=1`: the pads in pours of their net on stderr, by the necks that reach
/// them and by kind (`board::thermal::census`), and the thermal relief findings by code; with
/// `=relieve` also after `board::thermal::relieve` on a copy, with the time of both.
fn thermals(name: &str, what: &str, p: &Project) {
    let Ok(mode) = std::env::var("CADLAB_BENCH_THERMALS") else { return };
    let count = |p: &Project| {
        let t = Instant::now();
        let items = cadlab::board::copper_items(p);
        let fill = t.elapsed();
        let t = Instant::now();
        let rules = &p.board().rules;
        let js = cadlab::board::thermal::joints(p, &items);
        let c = cadlab::board::thermal::census(p, &js, rules.thermal_max_contact);
        if std::env::var_os("CADLAB_BENCH_VERBOSE").is_some() {
            for j in js.iter().filter(|j| j.solid(rules.thermal_max_contact).is_some()) {
                eprintln!("  joint {j:?}");
            }
        }
        let mut codes: std::collections::BTreeMap<String, usize> = Default::default();
        for d in cadlab::board::thermal::check(p, rules, &items, true) {
            *codes.entry(d.code.to_string()).or_default() += 1;
            if std::env::var_os("CADLAB_BENCH_VERBOSE").is_some() {
                eprintln!("  {}: {}", d.code, d.message);
            }
        }
        (c, codes, fill, t.elapsed())
    };
    let (c, codes, fill, t) = count(p);
    eprintln!("{name} {what}: {} [{codes:?}] (fill {fill:.2?}, joints and checks {t:.2?})", c.text());
    if mode == "relieve" {
        let mut q = p.clone();
        let t = Instant::now();
        let (r, _) = cadlab::board::thermal::relieve(&mut q, None, false);
        let changed = t.elapsed();
        let (c, codes, fill, t) = count(&q);
        eprintln!(
            "{name} {what} relieved: {} [{codes:?}] ({} zone(s) changed, {} setting(s) removed, {} kept, {} narrowed; \
             change {changed:.2?}, fill {fill:.2?}, joints and checks {t:.2?})",
            c.text(),
            r.zones.len(),
            r.removed.len(),
            r.kept.len(),
            r.narrowed.len()
        );
        eprintln!(
            "{name} {what} relieved, per pad: {} fitted, {} track(s), {} kept solid, {} left short, {} cut",
            r.fitted.len(),
            r.tracks.len(),
            r.solid.len(),
            r.reasons.len(),
            r.cut.len()
        );
        if std::env::var_os("CADLAB_BENCH_VERBOSE").is_some() {
            for l in r.fitted.iter().chain(&r.tracks).chain(&r.solid).chain(&r.cut) {
                eprintln!("  {l}");
            }
            for (k, v) in &r.reasons {
                eprintln!("  left {k}: {v}");
            }
        }
    }
}

/// Re-routes the corpus boards (see the module docs).
fn corpus(filters: &[String]) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::var_os("CADLAB_CORPUS_DIR").map_or(root.join("target/corpus"), PathBuf::from);
    let manifest: Manifest =
        toml::from_str(&std::fs::read_to_string(root.join("tests/corpus/projects.toml")).expect("manifest"))
            .expect("manifest syntax");
    let budget: u64 = std::env::var("CADLAB_BENCH_BUDGET_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(60_000);
    // guide's default is no seed: its nets shortest first.
    let guide = search_kind() == Some(SearchKind::Guide);
    let work: Option<u64> = std::env::var("CADLAB_BENCH_WORK").ok().and_then(|v| v.parse().ok());
    let verbose = std::env::var_os("CADLAB_BENCH_VERBOSE").is_some();
    let fr =
        std::env::var("CADLAB_BENCH_ROUTER").is_ok_and(|v| v == "freerouting").then(|| Freerouting::from_env(budget));
    println!(
        "| board | layers | connections | completion | failed | DRC unrouted | vias | length | time | work | budget out | DRC errors |\n|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    // Boards without a result where one is required: with freerouting (an oracle: nothing is
    // skipped silently) a board that is not fetched, or a run without a session it can import.
    let mut missing = 0;
    for e in
        manifest.project.iter().filter(|e| filters.is_empty() || filters.iter().any(|f| e.name.contains(f.as_str())))
    {
        let pcb = dir.join(&e.name).join(&e.pcb);
        let Ok(text) = std::fs::read_to_string(&pcb) else {
            eprintln!("{}: {} missing (run scripts/fetch-corpus.sh)", e.name, pcb.display());
            missing += 1;
            continue;
        };
        let read = |ext: &str| std::fs::read_to_string(pcb.with_extension(ext)).ok();
        let (pro, dru) = (read("kicad_pro"), read("kicad_dru"));
        let mut p = Project::new(&e.name);
        let k = rules::parse(pro.as_deref(), dru.as_deref()).map(|r| r.0).ok();
        let opts =
            BoardImportOptions { file_name: e.pcb.clone(), rules: k, origin: OriginMode::Aux, ..Default::default() };
        if let Err(err) = kicad_import::import(&mut p, &text, &opts) {
            eprintln!("{}: import failed: {}", e.name, err.message);
            continue;
        }
        // `CADLAB_BENCH_ORIGINAL=1`: the connections the project's own routing leaves open, instead
        // of a re-route (what no router is asked to do better than the designer did).
        if std::env::var_os("CADLAB_BENCH_ORIGINAL").is_some() {
            thermals(&e.name, "original", &p);
            let open = cadlab::board::ratsnest(&p);
            println!("{}: {} connections open in the original routing", e.name, open.len());
            for l in &open {
                println!("  {} {} -> {} ({:.2} mm)", l.net, l.from, l.to, l.length.0 as f64 / 1e6);
            }
            continue;
        }
        p.board_mut().tracks.clear();
        p.board_mut().vias.clear();
        // `CADLAB_BENCH_FANOUT=only`: the fanout alone (dog bones, crossovers, escapes), listed
        // around the component of `CADLAB_BENCH_DUMP`.
        if std::env::var("CADLAB_BENCH_FANOUT").is_ok_and(|v| v == "only") {
            match router::fanout(&p, &[], &[]) {
                Ok(f) => {
                    println!(
                        "{}: fanout of {} pads, {} tracks, {} vias",
                        e.name,
                        f.pads.len(),
                        f.tracks.len(),
                        f.vias.len()
                    );
                    p.board_mut().tracks.extend(f.tracks);
                    p.board_mut().vias.extend(f.vias);
                    if let Ok(spec) = std::env::var("CADLAB_BENCH_DUMP") {
                        dump(&p, &spec);
                    }
                }
                Err(err) => eprintln!("{}: {err}", e.name),
            }
            continue;
        }
        // `CADLAB_BENCH_ROUTER=freerouting`: the external router on the exported DSN, counted the
        // same way.
        if let Some(fr) = &fr {
            if !freerouting_row(fr, &e.name, e.layers, &p, verbose) {
                missing += 1;
            }
            continue;
        }
        let errors_before = drc_errors(&p);
        let ropts = RouteOptions {
            budget: if work.is_some() { None } else { Some(Duration::from_millis(budget)) },
            work: work.map_or_else(Default::default, WorkMeter::limited),
            seed: std::env::var("CADLAB_BENCH_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(u64::from(!guide)),
            search: search_kind(),
            fanout: std::env::var("CADLAB_BENCH_FANOUT").ok().map(|v| v != "0"),
            via_in_pad: std::env::var_os("CADLAB_BENCH_VIA_IN_PAD").is_some(),
            ..Default::default()
        };
        let t = Instant::now();
        let meter = ropts.work.clone();
        // `CADLAB_BENCH_TRACE=1`: a timeline of the router's phases and iteration summaries.
        let trace = std::env::var_os("CADLAB_BENCH_TRACE").is_some();
        let progress = |_: u64, _: Option<u64>, msg: &str| {
            if trace && !msg.contains(": routing ") && !msg.starts_with("gridless: ") {
                eprintln!("  [{:7.2} s {:7.1} M] {msg}", t.elapsed().as_secs_f64(), meter.used() as f64 / 1e6);
            }
        };
        let hooks = Hooks { progress: &progress, cancelled: &|| false };
        // `CADLAB_BENCH_NETS=a,b`: route only these nets.
        let scope = match std::env::var("CADLAB_BENCH_NETS") {
            Ok(v) => Scope::Nets(v.split(',').map(str::to_string).collect()),
            Err(_) => Scope::All,
        };
        let r = match router::route(&p, &scope, &ropts, &hooks) {
            Ok(r) => r,
            Err(err) => {
                eprintln!("{}: {err}", e.name);
                continue;
            }
        };
        let ms = t.elapsed().as_millis();
        let length: f64 = r.tracks.iter().map(|t| cadlab::board::track_length(t).0 as f64).sum::<f64>() / 1e6;
        let mut q = p.clone();
        q.board_mut().tracks.extend(r.tracks.iter().cloned());
        q.board_mut().vias.extend(r.vias.iter().cloned());
        let drc = drc_errors(&q).saturating_sub(errors_before);
        // The new errors by code: what the board has after routing, less what it had before.
        let mut by_code = routebench::drc_codes(&q);
        for (code, n) in routebench::drc_codes(&p) {
            if let Some(m) = by_code.get_mut(&code) {
                *m = m.saturating_sub(n);
            }
        }
        by_code.retain(|_, n| *n > 0);
        // What `route.status` sees on the result (the router's own count is per planned connection).
        let unrouted = cadlab::drc::check(&q).iter().filter(|d| d.code == "drc.unrouted").count();
        if verbose && drc > 0 {
            let before: std::collections::BTreeSet<String> = drc_messages(&p).into_iter().collect();
            for m in drc_messages(&q).into_iter().filter(|m| !before.contains(m)) {
                eprintln!("  DRC: {m}");
            }
        }
        if let Some(dir) = std::env::var_os("CADLAB_ROUTE_RENDER").map(PathBuf::from) {
            render(&q, &dir.join(format!("{}.png", e.name)), std::env::var("CADLAB_BENCH_AROUND").ok());
        }
        if let Ok(spec) = std::env::var("CADLAB_BENCH_DUMP") {
            dump(&q, &spec);
        }
        thermals(&e.name, "routed", &q);
        println!(
            "| {} | {} | {} | {:.1}% | {} | {} | {} | {:.1} mm | {:.1} s | {:.1} M | {} | {}{} |",
            e.name,
            e.layers,
            r.stats.connections,
            r.stats.completion,
            r.stats.failed,
            unrouted,
            r.stats.vias,
            length,
            ms as f64 / 1000.0,
            r.stats.work as f64 / 1e6,
            if r.stats.budget_exhausted { "yes" } else { "no" },
            drc,
            if by_code.is_empty() {
                String::new()
            } else {
                let codes: Vec<String> = by_code.iter().map(|(c, n)| format!("{n}× {c}")).collect();
                format!(" ({})", codes.join(", "))
            }
        );
        for n in &r.notes {
            eprintln!("  {}: note {}: {}", e.name, n.code, n.message);
        }
        if verbose {
            eprintln!("  stats: {}", serde_json::to_string(&r.stats).unwrap_or_default());
            eprintln!("  {}: {} of {} via(s) in SMD pads", e.name, routebench::vias_in_pads(&q), q.board().vias.len());
            // The vias and the track of the nets with zones apart: a pad of such a net has a via
            // to its plane whichever router routes it, and the others are where routers differ.
            let zoned: std::collections::BTreeSet<&str> = p
                .board()
                .zones
                .iter()
                .filter(|z| !cadlab::board::thermal::is_teardrop(z))
                .filter_map(|z| z.net.as_deref())
                .collect();
            let of_zone = |n: &Option<String>| n.as_deref().is_some_and(|n| zoned.contains(n));
            let zone_vias = r.vias.iter().filter(|v| of_zone(&v.net)).count();
            let zone_len: f64 = r
                .tracks
                .iter()
                .filter(|t| of_zone(&t.net))
                .map(|t| cadlab::board::track_length(t).0 as f64)
                .sum::<f64>()
                / 1e6;
            eprintln!(
                "  {}: nets with zones {zone_vias} via(s) and {zone_len:.1} mm, the others {} via(s) and {:.1} mm",
                e.name,
                r.vias.len() - zone_vias,
                length - zone_len
            );
            let mut by_net: BTreeMap<&str, usize> = BTreeMap::new();
            for v in r.vias.iter().filter(|v| of_zone(&v.net)) {
                *by_net.entry(v.net.as_deref().unwrap_or("")).or_default() += 1;
            }
            let by_net: Vec<String> = by_net.iter().map(|(n, c)| format!("{n} {c}")).collect();
            eprintln!("  {}: vias of the nets with zones: {}", e.name, by_net.join(", "));
            // The track that is outside the pads of its net: a router that ends a track at the
            // first point of its grid inside a pad and one that goes on to the pad's centre
            // differ by what is inside, which is copper on copper (D116).
            {
                let pads = cadlab::board::placed_pads(&q);
                let mut by_net: BTreeMap<&str, Vec<&cadlab::board::PlacedPad>> = BTreeMap::new();
                for pp in &pads {
                    if let Some(n) = pp.net.as_deref() {
                        by_net.entry(n).or_default().push(pp);
                    }
                }
                let mut outside = 0.0f64;
                let mut zone_outside = 0.0f64;
                for t in r.tracks.iter().filter(|t| t.mid.is_none()) {
                    let (a, b) = (t.start, t.end);
                    let len = ((b.x.0 - a.x.0) as f64).hypot((b.y.0 - a.y.0) as f64);
                    let at =
                        |s: f64| (a.x.0 as f64 + (b.x.0 - a.x.0) as f64 * s, a.y.0 as f64 + (b.y.0 - a.y.0) as f64 * s);
                    // By crossings of the pad's outline.
                    let holds = |pp: &cadlab::board::PlacedPad, (x, y): (f64, f64)| {
                        let ring = &pp.shape.outer.0;
                        let mut odd = false;
                        for k in 0..ring.len() {
                            let (p, q) = (ring[k], ring[(k + 1) % ring.len()]);
                            let (px, py, qx, qy) = (p.x as f64, p.y as f64, q.x as f64, q.y as f64);
                            if (py > y) != (qy > y) && px + (y - py) / (qy - py) * (qx - px) > x {
                                odd = !odd;
                            }
                        }
                        odd
                    };
                    let own: Vec<&&cadlab::board::PlacedPad> = t
                        .net
                        .as_deref()
                        .and_then(|n| by_net.get(n))
                        .map(|v| v.iter().filter(|pp| pp.layers.contains(&t.layer)).collect())
                        .unwrap_or_default();
                    // From each end, how far the track stays in the pad that holds the end.
                    let inside = |from: f64, to: f64| -> f64 {
                        let Some(pp) = own.iter().find(|pp| holds(pp, at(from))) else { return 0.0 };
                        if holds(pp, at(to)) {
                            return 1.0;
                        }
                        let (mut lo, mut hi) = (0.0f64, 1.0f64);
                        for _ in 0..24 {
                            let m = (lo + hi) / 2.0;
                            if holds(pp, at(from + (to - from) * m)) {
                                lo = m;
                            } else {
                                hi = m;
                            }
                        }
                        lo
                    };
                    let out = len * (1.0 - (inside(0.0, 1.0) + inside(1.0, 0.0)).min(1.0)) / 1e6;
                    outside += out;
                    if of_zone(&t.net) {
                        zone_outside += out;
                    }
                }
                eprintln!(
                    "  {}: track outside the pads of its net {outside:.1} mm, of the nets with zones {zone_outside:.1} mm",
                    e.name
                );
            }
            // By net, its track and its vias, for two routers' results side by side (D116).
            if let Some(dir) = std::env::var_os("CADLAB_BENCH_BY_NET").map(PathBuf::from) {
                let mut per: BTreeMap<&str, (f64, usize)> = BTreeMap::new();
                for t in &r.tracks {
                    per.entry(t.net.as_deref().unwrap_or("")).or_default().0 +=
                        cadlab::board::track_length(t).0 as f64 / 1e6;
                }
                for v in &r.vias {
                    per.entry(v.net.as_deref().unwrap_or("")).or_default().1 += 1;
                }
                let open: std::collections::BTreeSet<&str> = r
                    .connections
                    .iter()
                    .filter(|c| c.status == cadlab::router::ConnStatus::Failed)
                    .map(|c| c.net.as_str())
                    .collect();
                let text: String = per
                    .iter()
                    .map(|(n, (l, v))| {
                        format!(
                            "{n}\t{l:.3}\t{v}\t{}\t{}\n",
                            u8::from(of_zone(&Some(n.to_string()))),
                            u8::from(open.contains(n))
                        )
                    })
                    .collect();
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(dir.join(format!("{}.nets", e.name)), text);
                let mut text = String::new();
                for t in &r.tracks {
                    text += &format!(
                        "t\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                        t.net.as_deref().unwrap_or(""),
                        t.layer,
                        t.start.x.0,
                        t.start.y.0,
                        t.end.x.0,
                        t.end.y.0,
                        t.width.0
                    );
                }
                for v in &r.vias {
                    text += &format!(
                        "v\t{}\t{}\t{}\t{}\n",
                        v.net.as_deref().unwrap_or(""),
                        v.at.x.0,
                        v.at.y.0,
                        v.diameter.0
                    );
                }
                for pp in cadlab::board::placed_pads(&q) {
                    text += &format!(
                        "p\t{}\t{}.{}\t{}\t{}\t{}\n",
                        pp.net.as_deref().unwrap_or(""),
                        pp.refdes,
                        pp.number,
                        pp.center.x.0,
                        pp.center.y.0,
                        pp.layers.join(",")
                    );
                }
                let _ = std::fs::write(dir.join(format!("{}.tracks", e.name)), text);
            }
            // By net with zones, the pieces that the pads and the fills of the routed board are in
            // without the net's new tracks and vias, all and those with pads: one less is what
            // the router had to join, by a via to a plane or by a track.
            {
                use cadlab::board::ItemRef;
                let new: std::collections::BTreeSet<_> =
                    r.tracks.iter().map(|t| t.id).chain(r.vias.iter().map(|v| v.id)).collect();
                let mut items = cadlab::board::copper_items(&q);
                items.retain(|it| match &it.item {
                    ItemRef::Track(id) | ItemRef::Via(id) => !(new.contains(id) && of_zone(&it.net)),
                    _ => true,
                });
                let isl = cadlab::board::islands(&items);
                let mut all: BTreeMap<&str, std::collections::BTreeSet<usize>> = BTreeMap::new();
                let mut padded: BTreeMap<&str, std::collections::BTreeSet<usize>> = BTreeMap::new();
                for (it, &k) in items.iter().zip(&isl) {
                    let Some(n) = it.net.as_deref().filter(|n| zoned.contains(n)) else { continue };
                    all.entry(n).or_default().insert(k);
                    if matches!(it.item, ItemRef::Pad(..)) {
                        padded.entry(n).or_default().insert(k);
                    }
                }
                let list: Vec<String> = all
                    .iter()
                    .map(|(n, set)| format!("{n} {} ({} with pads)", set.len(), padded.get(n).map_or(0, |s| s.len())))
                    .collect();
                eprintln!("  {}: pieces of the nets with zones without their routes: {}", e.name, list.join(", "));
            }
        }
        let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
        for c in r.connections.iter().filter(|c| c.status == router::ConnStatus::Failed) {
            let reason = c.reason.clone().unwrap_or_default();
            if verbose {
                eprintln!("  {} {} -> {}: {} {:?}", c.net, c.from, c.to, reason, c.at);
            }
            // Group by the reason's kind: the text before the first colon or "blocked by".
            let kind = reason.split([':']).next().unwrap_or("").split(" from ").next().unwrap_or("").to_string();
            let kind = match reason.find("blocked by") {
                Some(i) => format!("{kind} / {}", reason[i..].split_whitespace().take(3).collect::<Vec<_>>().join(" ")),
                None => kind,
            };
            *reasons.entry(kind).or_default() += 1;
        }
        for (k, n) in &reasons {
            eprintln!("  {}: {n}× {k}", e.name);
        }
    }
    if fr.is_some() && missing > 0 {
        eprintln!("{missing} board(s) without a result of freerouting");
        std::process::exit(1);
    }
}

/// freerouting as an external process (`CADLAB_BENCH_ROUTER=freerouting` in the corpus mode;
/// docs/ROUTER.md, "freerouting on the corpus"). It is run on the DSN `export.dsn` writes and its
/// session is applied as `route.import_ses` applies it, so the result is counted by cadlab's
/// DRC like the native router's and guide's.
struct Freerouting {
    /// `java` (`CADLAB_ORACLE_JAVA`, else `$JAVA_HOME/bin/java`, else `java`).
    java: PathBuf,
    /// The released jar (`CADLAB_ORACLE_FREEROUTING`).
    jar: PathBuf,
    /// Processors the JVM may use and freerouting's `-mt` (`CADLAB_BENCH_FR_THREADS`, default 1).
    threads: u32,
    /// `-mp`, the most routing passes (`CADLAB_BENCH_FR_PASSES`; freerouting's own default without).
    passes: Option<u32>,
    /// freerouting's own time limit, `--router.job_timeout` (the budget of `CADLAB_BENCH_BUDGET_MS`;
    /// `CADLAB_BENCH_FR_TIMEOUT=none` leaves it out: freerouting then has its own default, 12 hours).
    timeout: Option<Duration>,
    /// The process is killed after this long (`CADLAB_BENCH_FR_KILL_S`, default 4 h): a run that
    /// does not end has no session and no result.
    kill: Duration,
    /// Pours written as planes (`CADLAB_BENCH_FR_PLANES=1`, `export.dsn {planes: true}`).
    planes: bool,
    /// More arguments, split at spaces (`CADLAB_BENCH_FR_ARGS`).
    extra: Vec<String>,
    /// Where the DSN, the session and freerouting's log are kept (`CADLAB_BENCH_KEEP`).
    keep: Option<PathBuf>,
    /// Count the kept sessions again instead of running freerouting (`CADLAB_BENCH_FR_REUSE=1`).
    reuse: bool,
}

impl Freerouting {
    fn from_env(budget_ms: u64) -> Freerouting {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let jar = PathBuf::from(
            var("CADLAB_ORACLE_FREEROUTING").expect("CADLAB_BENCH_ROUTER=freerouting needs CADLAB_ORACLE_FREEROUTING"),
        );
        assert!(jar.is_file(), "CADLAB_ORACLE_FREEROUTING={} is not a file", jar.display());
        let java = var("CADLAB_ORACLE_JAVA")
            .map(PathBuf::from)
            .or_else(|| var("JAVA_HOME").map(|h| PathBuf::from(h).join("bin/java")))
            .unwrap_or_else(|| PathBuf::from("java"));
        Freerouting {
            java,
            jar,
            threads: var("CADLAB_BENCH_FR_THREADS").and_then(|v| v.parse().ok()).unwrap_or(1),
            passes: var("CADLAB_BENCH_FR_PASSES").and_then(|v| v.parse().ok()),
            timeout: (var("CADLAB_BENCH_FR_TIMEOUT").as_deref() != Some("none"))
                .then(|| Duration::from_millis(budget_ms)),
            kill: Duration::from_secs(var("CADLAB_BENCH_FR_KILL_S").and_then(|v| v.parse().ok()).unwrap_or(4 * 3600)),
            planes: var("CADLAB_BENCH_FR_PLANES").is_some_and(|v| v != "0"),
            extra: var("CADLAB_BENCH_FR_ARGS")
                .map(|v| v.split_whitespace().map(String::from).collect())
                .unwrap_or_default(),
            keep: var("CADLAB_BENCH_KEEP").map(PathBuf::from),
            reuse: var("CADLAB_BENCH_FR_REUSE").is_some_and(|v| v != "0"),
        }
    }

    /// Runs freerouting on `dsn`: the wall time of the process (JVM start, reading the design
    /// and writing the session included), its console output, and whether it had to be killed.
    fn run(&self, dir: &std::path::Path, dsn: &std::path::Path, ses: &std::path::Path) -> (Duration, String, bool) {
        let mut cmd = std::process::Command::new(&self.java);
        cmd.arg(format!("-XX:ActiveProcessorCount={}", self.threads))
            // Its settings and logs go to the run's own directory, not the user's.
            .arg(format!("-Duser.home={}", dir.display()))
            .arg(format!("-Djava.io.tmpdir={}", dir.display()))
            .arg("-Djava.awt.headless=true")
            .arg("-jar")
            .arg(&self.jar)
            .arg("-de")
            .arg(dsn)
            .arg("-do")
            .arg(ses)
            .arg("-mt")
            .arg(self.threads.to_string())
            .arg("--gui.enabled=false")
            // No usage data leaves the machine, and no settings file of the user's is read.
            .arg("-da")
            .arg(format!("--user_data_path={}", dir.join("freerouting-data").display()))
            .arg(format!("--router.result_json={}", dir.join("result.json").display()));
        if let Some(n) = self.passes {
            cmd.arg("-mp").arg(n.to_string());
        }
        if let Some(t) = self.timeout {
            let s = t.as_secs();
            cmd.arg(format!("--router.job_timeout={:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60));
        }
        cmd.args(&self.extra).current_dir(dir);
        let log = dir.join("freerouting.log");
        let out = std::fs::File::create(&log).expect("log file");
        cmd.stdout(out.try_clone().expect("log file")).stderr(out).stdin(std::process::Stdio::null());
        let t = Instant::now();
        let mut child = cmd.spawn().unwrap_or_else(|e| panic!("running {}: {e}", self.java.display()));
        let mut killed = false;
        loop {
            match child.try_wait().expect("waiting for freerouting") {
                Some(_) => break,
                None if t.elapsed() > self.kill => {
                    let _ = child.kill();
                    let _ = child.wait();
                    killed = true;
                    break;
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        (t.elapsed(), std::fs::read_to_string(&log).unwrap_or_default(), killed)
    }
}

/// Whether a DRC error of the routed board is one a router cannot be held to through a DSN,
/// because the design file has no place for what it checks (docs/ROUTER.md, "Specctra DSN/SES",
/// limits); such errors are counted apart, under the returned name.
///
/// - By code: the minimum track width (the DSN gives each class its width, and a router that
///   necks a track down at a narrow pad has no lower bound to keep), the clearances of drilled
///   holes to copper and to each other, the copper-to-edge clearance, a keep-out's exception for
///   some nets.
/// - A clearance error whose gap is at least the clearance the DSN gives the two nets (the
///   larger of their classes'): the rule broken is one of a pad, a footprint or a custom rule,
///   which the DSN does not have. An error whose gap cannot be read counts as carried.
/// - A short or a clearance error with a copper drawing of the board or of a footprint: the
///   DSN has the pads of a footprint and no other copper of it, and no drawing of the board.
fn not_in_dsn(d: &cadlab::Diagnostic, dsn: &cadlab::specctra::dsn::Dsn) -> Option<String> {
    use cadlab::ObjectRef;
    const BY_CODE: &[&str] =
        &["drc.track_width", "drc.hole_clearance", "drc.hole_to_hole", "drc.copper_to_edge", "drc.keepout_net"];
    let code = d.code.as_ref();
    if BY_CODE.contains(&code) {
        return Some(code.to_string());
    }
    // `graphic#12` is a drawing of the board, `copper:JP1/0` one of a footprint.
    if (code == "drc.short" || code == "drc.clearance")
        && (d.message.contains("graphic#") || d.message.contains("copper:"))
    {
        return Some(format!("{code} with a copper drawing"));
    }
    if code == "drc.clearance" {
        let default = dsn.rule.clearance.map_or(0, |c| c.0);
        let of = |net: &str| {
            dsn.classes.iter().find(|c| c.nets.iter().any(|n| n == net)).and_then(|c| c.rule.clearance).map(|c| c.0)
        };
        let nets: Vec<&str> = d
            .subjects
            .iter()
            .filter_map(|s| match s {
                ObjectRef::Net(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        let carried = nets.iter().filter_map(|n| of(n)).chain([default]).max().unwrap_or(default);
        // "... are 0.313119mm apart on F.Cu, clearance is 0.5mm"
        let gap = d
            .message
            .split(" are ")
            .nth(1)
            .and_then(|r| r.split("mm apart").next())
            .and_then(|g| g.trim().parse::<f64>().ok())
            .map(|mm| (mm * 1e6).round() as i64)?;
        // The DRC's own tolerance is under a micrometre.
        if gap + 1_000 >= carried {
            return Some("drc.clearance by a local or custom rule".to_string());
        }
    }
    None
}

/// Routes one corpus board (its routing already removed) with freerouting and prints its row;
/// `false` when there is no result to count (no session, or one that cannot be imported).
fn freerouting_row(fr: &Freerouting, name: &str, layers: u8, p: &Project, verbose: bool) -> bool {
    use cadlab::specctra::export::{Options, export};
    use cadlab::specctra::ses::{ImportOptions, Session, plan};
    let tmp = tempfile::tempdir().expect("temporary directory");
    let dir = match &fr.keep {
        Some(k) => {
            let d = k.join(name);
            std::fs::create_dir_all(&d).expect("CADLAB_BENCH_KEEP");
            d
        }
        None => tmp.path().to_path_buf(),
    };
    let unrouted = |p: &Project| cadlab::drc::check(p).iter().filter(|d| d.code == "drc.unrouted").count();
    let connections = unrouted(p);
    let x = export(p, name, &Options { planes: fr.planes, ..Options::default() });
    for w in &x.warnings {
        eprintln!("  {name}: DSN: {w}");
    }
    for (k, nets) in &x.net_exceptions {
        eprintln!("  {name}: DSN: keep-out `{k}` written without its exception for {}", nets.join(", "));
    }
    eprintln!("  {name}: DSN: not written: {}", unwritten(p));
    let (dsn, ses) = (dir.join(format!("{name}.dsn")), dir.join(format!("{name}.ses")));
    // `CADLAB_BENCH_FR_REUSE=1` with `CADLAB_BENCH_KEEP`: the kept session is counted again
    // without running freerouting (its wall time from the kept `time.txt`).
    let kept = fr.reuse.then(|| std::fs::read_to_string(dir.join("time.txt")).ok()).flatten();
    let (time, log, killed) = match kept.and_then(|t| t.trim().parse::<f64>().ok()).filter(|_| ses.is_file()) {
        Some(secs) => (
            Duration::from_secs_f64(secs),
            std::fs::read_to_string(dir.join("freerouting.log")).unwrap_or_default(),
            false,
        ),
        None => {
            std::fs::write(&dsn, x.dsn.write()).expect("writing the DSN");
            let _ = std::fs::remove_file(&ses);
            let _ = std::fs::remove_file(dir.join("result.json"));
            let r = fr.run(&dir, &dsn, &ses);
            let _ = std::fs::write(dir.join("time.txt"), format!("{:.3}\n", r.0.as_secs_f64()));
            r
        }
    };
    if verbose {
        for l in log.lines() {
            eprintln!("  freerouting: {l}");
        }
    }
    let secs = time.as_secs_f64();
    let Ok(text) = std::fs::read_to_string(&ses) else {
        println!(
            "| {name} | {layers} | {connections} | - | - | - | - | - | {secs:.1} s | - | {} | no session |",
            if killed { "killed" } else { "-" }
        );
        return false;
    };
    let session = match Session::parse(&text) {
        Ok(s) => s,
        Err(e) => {
            println!("| {name} | {layers} | {connections} | - | - | - | - | - | {secs:.1} s | - | - | session: {e} |");
            return false;
        }
    };
    let import = match plan(p, &session, &ImportOptions::default()) {
        Ok(i) => i,
        Err(e) => {
            println!("| {name} | {layers} | {connections} | - | - | - | - | - | {secs:.1} s | - | - | import: {e} |");
            return false;
        }
    };
    for w in &import.warnings {
        eprintln!("  {name}: session: {}: {}", w.code, w.message);
    }
    // As `route.import_ses` applies it.
    let mut q = p.clone();
    q.board_mut().tracks.retain(|t| !import.remove_tracks.contains(&t.id));
    q.board_mut().vias.retain(|v| !import.remove_vias.contains(&v.id));
    for mut t in import.tracks {
        t.id = q.alloc_id();
        q.board_mut().tracks.push(t);
    }
    for mut v in import.vias {
        v.id = q.alloc_id();
        q.board_mut().vias.push(v);
    }
    let open = unrouted(&q);
    let length: f64 = q.board().tracks.iter().map(|t| cadlab::board::track_length(t).0 as f64).sum::<f64>() / 1e6;
    // The errors the routing adds: those of the routed board that the board did not have,
    // those the DSN could not have told the router apart from the others.
    let had: std::collections::BTreeSet<String> = drc_messages(p).into_iter().collect();
    let (mut carried, mut unseen): (BTreeMap<String, usize>, BTreeMap<String, usize>) = Default::default();
    for d in cadlab::drc::check(&q) {
        let m = format!("{}: {}", d.code, d.message);
        if d.severity != cadlab::Severity::Error || d.code == "drc.unrouted" || had.contains(&m) {
            continue;
        }
        let class = not_in_dsn(&d, &x.dsn);
        if verbose {
            let at = d
                .location
                .map_or(String::new(), |a| format!(" at {:.3} {:.3}", a.x.0 as f64 / 1e6, a.y.0 as f64 / 1e6));
            eprintln!("  DRC{}: {m}{at}", if class.is_some() { " (not in the DSN)" } else { "" });
        }
        match class {
            Some(c) => *unseen.entry(c).or_default() += 1,
            None => *carried.entry(d.code.to_string()).or_default() += 1,
        }
    }
    let list = |codes: &BTreeMap<String, usize>| {
        let c: Vec<String> = codes.iter().map(|(c, n)| format!("{n}× {c}")).collect();
        let n: usize = codes.values().sum();
        if c.is_empty() { n.to_string() } else { format!("{n} ({})", c.join(", ")) }
    };
    if verbose {
        for l in cadlab::board::ratsnest(&q) {
            eprintln!("  open: {} {} -> {} ({:.2} mm)", l.net, l.from, l.to, l.length.0 as f64 / 1e6);
        }
        eprintln!("  {name}: {} of {} via(s) in SMD pads", routebench::vias_in_pads(&q), q.board().vias.len());
    }
    if let Some(dir) = std::env::var_os("CADLAB_ROUTE_RENDER").map(PathBuf::from) {
        render(&q, &dir.join(format!("{name}.png")), std::env::var("CADLAB_BENCH_AROUND").ok());
    }
    let own = own_count(&dir.join("result.json"));
    if let Some((end, routed, state)) = &own {
        eprintln!(
            "  {name}: freerouting's own count: {end} open at the end, {} after its routing stage; job {state}",
            routed.map_or("-".to_string(), |n| n.to_string())
        );
    }
    let state = own.as_ref().map_or("-".to_string(), |o| o.2.clone());
    let own = own.map_or("-".to_string(), |o| o.0.to_string());
    println!(
        "| {name} | {layers} | {connections} | {:.1}% | {own} | {open} | {} | {length:.1} mm | {secs:.1} s | - | {} | {}; not in the DSN: {} |",
        if connections == 0 { 100.0 } else { 100.0 * (connections.saturating_sub(open)) as f64 / connections as f64 },
        q.board().vias.len(),
        if killed { "killed" } else { state.as_str() },
        list(&carried),
        list(&unseen),
    );
    true
}

/// What of a board's rules and copper a DSN has no place for, counted: custom rules by what
/// they set, pads with a clearance of their own (or their footprint's), copper drawings of the
/// board and of footprints, and the board's minimums.
fn unwritten(p: &Project) -> String {
    use cadlab::model::footprint::GraphicLayer;
    let b = p.board();
    let rules = &b.custom_rules;
    let (mut pads, mut drawings) = (0, 0);
    for refdes in b.footprints.keys() {
        let Some(fp) = cadlab::board::footprint_for(p, refdes) else { continue };
        pads += fp.pads.iter().filter(|pad| pad.overrides.clearance.or(fp.overrides.clearance).is_some()).count();
        drawings += fp.graphics.iter().filter(|g| g.layer == GraphicLayer::Copper).count();
    }
    let mm = |n: cadlab::Nm| n.0 as f64 / 1e6;
    format!(
        "{} custom rule(s) ({} with a clearance, {} with a track width, {} forbidding items), {} pad(s) with a \
         clearance of their own, {} copper drawing(s) of the board and {} of footprints, minimum track width {} mm, \
         copper to hole {} mm, hole to hole {} mm, copper to edge {} mm",
        rules.len(),
        rules.iter().filter(|r| r.clearance.is_some()).count(),
        rules.iter().filter(|r| r.track_width.is_some()).count(),
        rules.iter().filter(|r| !r.disallow.is_empty()).count(),
        pads,
        b.graphics.iter().filter(|g| b.is_copper(&g.layer)).count(),
        drawings,
        mm(b.rules.min_track_width),
        mm(b.rules.hole_clearance),
        mm(b.rules.hole_to_hole),
        mm(b.rules.copper_to_edge),
    )
}

/// What freerouting says of its own run, from the manifest it writes (`--router.result_json`):
/// the connections it counts open at the end and after its routing stage alone (its
/// optimization comes after), and how the job ended (`COMPLETED`, `TIMED_OUT`, ...).
fn own_count(manifest: &std::path::Path) -> Option<(u64, Option<u64>, String)> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(manifest).ok()?).ok()?;
    let open = |v: &serde_json::Value| v["board_statistics"]["connections"]["incomplete_count"].as_u64();
    Some((open(&v)?, open(&v["phases"]["autorouter"]["after"]), v["final_state"].as_str().unwrap_or("-").to_string()))
}

/// `CADLAB_BENCH_DUMP=<refdes>[.<pad>][,<margin mm>]`: the pads, tracks and vias of the routed
/// board within the margin (default 2 mm) of a component's pads (or of one pad), in mm, for
/// looking at a failing neighborhood.
fn dump(p: &Project, spec: &str) {
    let (what, margin) = spec.split_once(',').unwrap_or((spec, "2"));
    let m = (margin.parse::<f64>().unwrap_or(2.0) * 1e6) as i64;
    let (refdes, pad) = what.split_once('.').map_or((what, None), |(r, n)| (r, Some(n)));
    let pads = cadlab::board::placed_pads(p);
    let mine: Vec<_> = pads.iter().filter(|q| q.refdes == refdes && pad.is_none_or(|n| q.number == n)).collect();
    if mine.is_empty() {
        eprintln!("dump: no pad matches {what}");
        return;
    }
    let xs = || mine.iter().map(|q| q.center.x.0);
    let ys = || mine.iter().map(|q| q.center.y.0);
    let (x0, x1) = (xs().min().unwrap_or(0) - m, xs().max().unwrap_or(0) + m);
    let (y0, y1) = (ys().min().unwrap_or(0) - m, ys().max().unwrap_or(0) + m);
    let inside = |q: cadlab::Point| (x0..=x1).contains(&q.x.0) && (y0..=y1).contains(&q.y.0);
    let mm = |v: i64| v as f64 / 1e6;
    eprintln!("dump {what}: x {:.3}..{:.3} y {:.3}..{:.3}", mm(x0), mm(x1), mm(y0), mm(y1));
    for q in pads.iter().filter(|q| inside(q.center)) {
        let Some(b) = q.shape.bbox() else { continue };
        eprintln!(
            "  pad {}.{} net {} at {:.4} {:.4} box {:.3} x {:.3} layers {:?}",
            q.refdes,
            q.number,
            q.net.as_deref().unwrap_or("-"),
            mm(q.center.x.0),
            mm(q.center.y.0),
            mm(b.max.x - b.min.x),
            mm(b.max.y - b.min.y),
            q.layers
        );
    }
    for t in p.board().tracks.iter().filter(|t| inside(t.start) || inside(t.end)) {
        eprintln!(
            "  track {} {} ({:.4} {:.4}) -> ({:.4} {:.4}) w {:.3}",
            t.net.as_deref().unwrap_or("-"),
            t.layer,
            mm(t.start.x.0),
            mm(t.start.y.0),
            mm(t.end.x.0),
            mm(t.end.y.0),
            mm(t.width.0)
        );
    }
    for v in p.board().vias.iter().filter(|v| inside(v.at)) {
        eprintln!(
            "  via {} ({:.4} {:.4}) d {:.3} drill {:.3}",
            v.net.as_deref().unwrap_or("-"),
            mm(v.at.x.0),
            mm(v.at.y.0),
            mm(v.diameter.0),
            mm(v.drill.0)
        );
    }
}

/// Renders a routed board (`CADLAB_ROUTE_RENDER`), cropped around a component
/// (`CADLAB_BENCH_AROUND`) if given.
fn render(p: &Project, path: &std::path::Path, around: Option<String>) {
    use cadlab::command::{Registry, RunOptions, Session};
    let r = Registry::with_builtins();
    let mut s = Session::new();
    s.project = Some(p.clone());
    let mut args = serde_json::json!({"path": path, "px_per_mm": if around.is_some() { 120 } else { 20 }});
    if let Some(a) = around {
        args["around"] = a.into();
        args["margin"] = "1.5mm".into();
    }
    if let Err(f) = r.execute(&mut s, "render.board", args, RunOptions::default()) {
        eprintln!("render failed: {}", f.error);
    }
}

/// The DRC errors (other than unrouted connections) as `code: message` lines.
fn drc_messages(p: &Project) -> Vec<String> {
    cadlab::drc::check(p)
        .into_iter()
        .filter(|d| d.severity == cadlab::Severity::Error && d.code != "drc.unrouted")
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

fn drc_errors(p: &Project) -> usize {
    cadlab::drc::check(p)
        .into_iter()
        .filter(|d| d.severity == cadlab::Severity::Error && d.code != "drc.unrouted")
        .count()
}
