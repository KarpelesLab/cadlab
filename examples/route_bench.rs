//! Router benchmark suite (`tests/common/routebench.rs`, docs/ROUTER.md "Benchmarks"):
//! `cargo run --release --example route_bench [names...]`.
//!
//! Prints a Markdown table: completion, vias, wirelength, segments, sharp corners, runtime and
//! DRC errors per board. With `CADLAB_ORACLE_FREEROUTING=/path/freerouting.jar` every board is
//! also routed by freerouting (external process, DSN/SES) for comparison. `CADLAB_ROUTE_RENDER=<dir>`
//! renders cadlab's results; `CADLAB_BENCH_BUDGET_MS` sets the time budget (default 120000);
//! `CADLAB_BENCH_ROUTER=grid|gridless|auto` picks the search (default: the router's default) and
//! `CADLAB_BENCH_ROUTER=guide` the guide crate's router in its place (D85; here and in the corpus
//! mode). guide takes no seed, no budget and no work budget, so those are not passed to it: bound
//! a run from outside (`timeout 1200 route_bench --corpus <board>`), one board per process.
//! `CADLAB_BENCH_VIA_IN_PAD=1` lets guide place vias in pads, as it does on its own; without it
//! the adapter forbids them, as the native router does. With
//! `CADLAB_BENCH_VERBOSE=1` both modes count the vias in surface-mount pads of their net.
//! `CADLAB_BENCH_SEED=<n>` sets the seed (default 1).
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
//! `CADLAB_BENCH_TRACE=1` prints a timeline of the router's phases and negotiation iterations;
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
        other => panic!("CADLAB_BENCH_ROUTER: unknown router `{other}` (grid, gridless, auto, guide)"),
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
    // guide has no work budget (and no time budget): it runs until it is done.
    let guide = search_kind() == Some(SearchKind::Guide);
    let work: Option<u64> = std::env::var("CADLAB_BENCH_WORK").ok().and_then(|v| v.parse().ok()).filter(|_| !guide);
    let verbose = std::env::var_os("CADLAB_BENCH_VERBOSE").is_some();
    println!(
        "| board | layers | connections | completion | failed | DRC unrouted | vias | length | time | work | budget out | DRC errors |\n|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for e in
        manifest.project.iter().filter(|e| filters.is_empty() || filters.iter().any(|f| e.name.contains(f.as_str())))
    {
        let pcb = dir.join(&e.name).join(&e.pcb);
        let Ok(text) = std::fs::read_to_string(&pcb) else {
            eprintln!("{}: {} missing (run scripts/fetch-corpus.sh)", e.name, pcb.display());
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
        let errors_before = drc_errors(&p);
        let ropts = RouteOptions {
            budget: if work.is_some() { None } else { Some(Duration::from_millis(budget)) },
            work: work.map_or_else(Default::default, WorkMeter::limited),
            seed: std::env::var("CADLAB_BENCH_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(1),
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
