//! Router benchmark suite (`tests/common/routebench.rs`, docs/ROUTER.md "Benchmarks"):
//! `cargo run --release --example route_bench [names...]`.
//!
//! Prints a Markdown table: completion, vias, wirelength, segments, sharp corners, runtime and
//! DRC errors per board. With `CADLAB_ORACLE_FREEROUTING=/path/freerouting.jar` every board is
//! also routed by freerouting (external process, DSN/SES) for comparison. `CADLAB_ROUTE_RENDER=<dir>`
//! renders cadlab's results; `CADLAB_BENCH_BUDGET_MS` sets the time budget (default 120000);
//! `CADLAB_BENCH_ROUTER=grid|gridless|auto` picks the search (default: the router's default).
//!
//! **Corpus re-route mode**: `route_bench --corpus [names...]` imports the boards of the
//! open-source corpus (`tests/corpus/projects.toml`, fetched by `scripts/fetch-corpus.sh` into
//! `CADLAB_CORPUS_DIR`, default `target/corpus`; circuits built from the boards, no KiCad needed),
//! rips every track and via (zones stay), routes again and prints completion, vias, length, time
//! and DRC errors per board, with the failed connections grouped by reason
//! (`CADLAB_BENCH_VERBOSE=1` lists each one). The budget defaults to 60 s here.

#[path = "../tests/common/routebench.rs"]
mod routebench;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cadlab::kicad_import::{self, BoardImportOptions, OriginMode, rules};
use cadlab::model::Project;
use cadlab::router::{self, Hooks, RouteOptions, Scope, SearchKind};

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
        other => panic!("CADLAB_BENCH_ROUTER: unknown router `{other}` (grid, gridless, auto)"),
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
    let verbose = std::env::var_os("CADLAB_BENCH_VERBOSE").is_some();
    println!(
        "| board | layers | connections | completion | failed | vias | length | time | budget out | DRC errors |\n|---|---|---|---|---|---|---|---|---|---|"
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
        p.board_mut().tracks.clear();
        p.board_mut().vias.clear();
        let errors_before = drc_errors(&p);
        let ropts = RouteOptions {
            budget: Some(Duration::from_millis(budget)),
            seed: 1,
            search: search_kind(),
            fanout: std::env::var("CADLAB_BENCH_FANOUT").ok().map(|v| v != "0"),
            ..Default::default()
        };
        let t = Instant::now();
        let r = match router::route(&p, &Scope::All, &ropts, &Hooks::none()) {
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
        if let Some(dir) = std::env::var_os("CADLAB_ROUTE_RENDER").map(PathBuf::from) {
            render(&q, &dir.join(format!("{}.png", e.name)), std::env::var("CADLAB_BENCH_AROUND").ok());
        }
        println!(
            "| {} | {} | {} | {:.1}% | {} | {} | {:.1} mm | {:.1} s | {} | {} |",
            e.name,
            e.layers,
            r.stats.connections,
            r.stats.completion,
            r.stats.failed,
            r.stats.vias,
            length,
            ms as f64 / 1000.0,
            if r.stats.budget_exhausted { "yes" } else { "no" },
            drc
        );
        if verbose {
            eprintln!("  stats: {}", serde_json::to_string(&r.stats).unwrap_or_default());
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

fn drc_errors(p: &Project) -> usize {
    cadlab::drc::check(p)
        .into_iter()
        .filter(|d| d.severity == cadlab::Severity::Error && d.code != "drc.unrouted")
        .count()
}
