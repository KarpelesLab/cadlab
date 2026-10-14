//! Router benchmark suite (`tests/common/routebench.rs`, docs/ROUTER.md "Benchmarks").
//!
//! - `route_benchmark_regression` (`#[ignore]`d, run in release by the `bench` CI job): routes
//!   every board whose result does not depend on the time budget and compares it with the
//!   committed baseline `tests/golden/bench/route.json`. It fails on any DRC error, a lower
//!   completion, more vias or a longer total length than the tolerances below, an exhausted
//!   budget, or a large slowdown (`tests/common/bench.rs`). The report goes to
//!   `$CARGO_TARGET_TMPDIR/bench/route.{json,md}`. `CADLAB_BLESS=1` rewrites the baseline
//!   (measure it in release with `RAYON_NUM_THREADS=1`, like CI):
//!   `RAYON_NUM_THREADS=1 cargo test --release --test route_bench -- --ignored --nocapture route_benchmark_regression`.
//! - `route_baseline_matches_the_suite` (default run): the baseline lists exactly those boards,
//!   and the smallest one routes exactly as recorded.
//! - `route_benchmark_suite` (`#[ignore]`d): the full table, with the budget-bound `big-4l` and
//!   freerouting when `CADLAB_ORACLE_FREEROUTING` is set; asserts zero DRC errors.
//!   `cargo run --release --example route_bench` prints the table alone.

use std::collections::BTreeMap;

#[path = "common/bench.rs"]
mod bench;
#[path = "common/routebench.rs"]
mod routebench;

/// Boards whose routing runs into the time budget, so their result depends on the machine:
/// left out of the regression check.
const BUDGET_BOUND: &[&str] = &["big-4l"];

/// Budget of the regression runs: far above what any checked board needs, so results are
/// deterministic (an exhausted budget fails the check).
const REGRESSION_BUDGET_MS: u64 = 600_000;

/// Vias may grow by this share of the baseline (or [`VIA_SLACK_MIN`], whichever is larger).
const VIA_SLACK: f64 = 0.05;
/// Smallest via slack.
const VIA_SLACK_MIN: usize = 2;
/// Total track length may grow by this share of the baseline.
const LENGTH_SLACK: f64 = 0.02;

const BASELINE: &str = "route.json";

/// The metrics of one board, as recorded in the baseline.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Outcome {
    connections: u64,
    unrouted: u64,
    vias: usize,
    segments: usize,
    /// Total track length (µm).
    length_um: i64,
    sharp_corners: usize,
    drc_errors: usize,
    /// Routing time (ms, release build, `RAYON_NUM_THREADS=1`); compared by the timing rule only.
    ms: u64,
}

impl Outcome {
    fn of(m: &routebench::Metrics) -> Self {
        Outcome {
            connections: m.connections,
            unrouted: m.unrouted,
            vias: m.vias,
            segments: m.segments,
            length_um: (m.length_mm * 1000.0).round() as i64,
            sharp_corners: m.sharp_corners,
            drc_errors: m.drc_errors,
            ms: m.ms as u64,
        }
    }

    fn routed_share(&self) -> f64 {
        if self.connections == 0 { 1.0 } else { (self.connections - self.unrouted) as f64 / self.connections as f64 }
    }

    /// Everything but the time.
    fn same_outcome(&self, other: &Outcome) -> bool {
        Outcome { ms: 0, ..self.clone() } == Outcome { ms: 0, ..other.clone() }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Baseline {
    /// How the baseline was measured.
    measured: String,
    cases: BTreeMap<String, Outcome>,
}

/// Why `cur` is a regression from `base` (empty: none). Timing is checked separately.
fn regressions(base: &Outcome, cur: &Outcome, budget_exhausted: bool) -> Vec<String> {
    let mut out = Vec::new();
    if cur.drc_errors > 0 {
        out.push(format!("{} DRC errors", cur.drc_errors));
    }
    if budget_exhausted {
        out.push("the time budget ran out (the result is no longer deterministic)".into());
    }
    if cur.unrouted > base.unrouted || cur.routed_share() < base.routed_share() - 1e-9 {
        out.push(format!(
            "completion dropped: {}/{} unrouted, baseline {}/{}",
            cur.unrouted, cur.connections, base.unrouted, base.connections
        ));
    }
    let via_limit = base.vias + ((base.vias as f64 * VIA_SLACK).ceil() as usize).max(VIA_SLACK_MIN);
    if cur.vias > via_limit {
        out.push(format!("{} vias, baseline {} (limit {via_limit})", cur.vias, base.vias));
    }
    let length_limit = (base.length_um as f64 * (1.0 + LENGTH_SLACK)).round() as i64;
    if cur.length_um > length_limit {
        out.push(format!(
            "length {:.1} mm, baseline {:.1} mm (limit {:.1} mm)",
            cur.length_um as f64 / 1000.0,
            base.length_um as f64 / 1000.0,
            length_limit as f64 / 1000.0
        ));
    }
    out
}

fn checked_cases() -> Vec<routebench::Case> {
    routebench::cases().into_iter().filter(|c| !BUDGET_BOUND.contains(&c.name)).collect()
}

#[test]
#[ignore = "minutes in debug builds; the `bench` CI job runs it in release"]
fn route_benchmark_regression() {
    let baseline: Option<Baseline> = bench::read_baseline(BASELINE);
    let bless = bench::blessing();
    assert!(bless || baseline.is_some(), "no baseline: run with CADLAB_BLESS=1 (see the module docs)");
    let mut results = BTreeMap::new();
    let mut failures = Vec::new();
    let mut rows = Vec::new();
    let mut times = Vec::new();
    let mut table = String::from(
        "| board | connections | unrouted | vias | length | segments | DRC errors | baseline (unrouted / vias / length) | |\n\
         |---|---|---|---|---|---|---|---|---|\n",
    );
    for case in checked_cases() {
        let m = routebench::run_cadlab(&case, REGRESSION_BUDGET_MS, None);
        eprintln!("{}", routebench::row(&case, &m));
        let cur = Outcome::of(&m);
        let base = baseline.as_ref().and_then(|b| b.cases.get(case.name));
        let problems = match base {
            Some(b) => regressions(b, &cur, m.budget_exhausted),
            None if bless => vec![],
            None => vec!["not in the baseline (CADLAB_BLESS=1 adds it)".into()],
        };
        let changed = base.is_some_and(|b| !b.same_outcome(&cur));
        let status = if !problems.is_empty() {
            problems.join("; ")
        } else if changed {
            "changed within tolerance (CADLAB_BLESS=1 records it)".into()
        } else {
            "ok".into()
        };
        table.push_str(&format!(
            "| {} | {} | {} | {} | {:.1} mm | {} | {} | {} | {} |\n",
            case.name,
            cur.connections,
            cur.unrouted,
            cur.vias,
            cur.length_um as f64 / 1000.0,
            cur.segments,
            cur.drc_errors,
            base.map_or("—".into(), |b| format!(
                "{} / {} / {:.1} mm",
                b.unrouted,
                b.vias,
                b.length_um as f64 / 1000.0
            )),
            status
        ));
        failures.extend(problems.iter().map(|p| format!("{}: {p}", case.name)));
        times.push((case.name.to_string(), m.ms as f64));
        rows.push(serde_json::json!({
            "name": case.name,
            "measured": cur,
            "baseline": base,
            "budget_exhausted": m.budget_exhausted,
            "problems": problems,
            "changed": changed,
        }));
        results.insert(case.name.to_string(), cur);
    }
    let base_times: BTreeMap<String, f64> =
        baseline.as_ref().map(|b| b.cases.iter().map(|(n, r)| (n.clone(), r.ms as f64)).collect()).unwrap_or_default();
    let (verdicts, median) = bench::check_timings(&times, &base_times);
    failures.extend(verdicts.iter().filter_map(|v| v.failure.as_ref().map(|f| format!("{} (time): {f}", v.name))));
    let markdown =
        format!("## Router benchmark\n\n{table}\n### Routing times\n\n{}", bench::timing_table(&verdicts, median));
    let report = serde_json::json!({
        "profile": bench::profile(),
        "rayon_threads": std::env::var("RAYON_NUM_THREADS").ok(),
        "cases": rows,
        "timings": verdicts,
        "median_ratio": median,
        "failures": failures,
    });
    bench::write_report("route", &report, &markdown);
    eprintln!("{markdown}");
    if bless {
        let measured = format!(
            "{} build, RAYON_NUM_THREADS={}, budget {REGRESSION_BUDGET_MS} ms, seed 1",
            bench::profile(),
            std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "unset".into())
        );
        bench::write_baseline(BASELINE, &Baseline { measured, cases: results });
        return;
    }
    assert!(failures.is_empty(), "router regressions:\n{}", failures.join("\n"));
}

#[test]
fn route_baseline_matches_the_suite() {
    let Some(baseline) = bench::read_baseline::<Baseline>(BASELINE) else {
        assert!(bench::blessing(), "tests/golden/bench/{BASELINE} missing (see tests/route_bench.rs)");
        return;
    };
    let names: Vec<&str> = checked_cases().iter().map(|c| c.name).collect();
    let mut want = names.clone();
    want.sort_unstable();
    assert_eq!(baseline.cases.keys().map(String::as_str).collect::<Vec<_>>(), want, "baseline boards");
    // The smallest board routes in well under a second even in debug builds: exactly as recorded.
    let case = checked_cases().into_iter().next().unwrap();
    let m = routebench::run_cadlab(&case, REGRESSION_BUDGET_MS, None);
    let cur = Outcome::of(&m);
    let base = &baseline.cases[case.name];
    assert!(regressions(base, &cur, m.budget_exhausted).is_empty(), "{}: {cur:?}, baseline {base:?}", case.name);
    assert!(
        base.same_outcome(&cur),
        "{}: routed differently from tests/golden/bench/{BASELINE}: {cur:?}, baseline {base:?} \
         (rebless in release, see tests/route_bench.rs)",
        case.name
    );
}

#[test]
#[ignore]
fn route_benchmark_suite() {
    let render = std::env::var_os("CADLAB_ROUTE_RENDER").map(std::path::PathBuf::from);
    let fr = routebench::freerouting_tool();
    eprintln!("{}", routebench::HEADER);
    for case in routebench::cases() {
        let m = routebench::run_cadlab(&case, 120_000, render.as_deref());
        eprintln!("{}", routebench::row(&case, &m));
        assert_eq!(m.drc_errors, 0, "{}", case.name);
        if let Some(tool) = fr.as_ref().filter(|_| case.freerouting) {
            eprintln!("{}", routebench::row(&case, &routebench::run_freerouting(&case, tool)));
        }
    }
}
