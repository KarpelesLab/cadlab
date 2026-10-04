//! Router benchmark suite (`tests/common/routebench.rs`, docs/ROUTER.md "Benchmarks"):
//! `cargo run --release --example route_bench [names...]`.
//!
//! Prints a Markdown table: completion, vias, wirelength, segments, sharp corners, runtime and
//! DRC errors per board. With `CADLAB_ORACLE_FREEROUTING=/path/freerouting.jar` every board is
//! also routed by freerouting (external process, DSN/SES) for comparison. `CADLAB_ROUTE_RENDER=<dir>`
//! renders cadlab's results; `CADLAB_BENCH_BUDGET_MS` sets the time budget (default 120000).

#[path = "../tests/common/routebench.rs"]
mod routebench;

fn main() {
    let filters: Vec<String> = std::env::args().skip(1).collect();
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
