//! Benchmark baselines tracked in CI (docs/TESTING.md "Benchmarks in CI", DECISIONS D43):
//! committed baselines in `tests/golden/bench/`, machine-readable reports written to
//! `$CARGO_TARGET_TMPDIR/bench/` (uploaded by the `bench` CI job), and the timing rule shared by
//! the router benchmark (`tests/route_bench.rs`) and the large-board timings (`tests/perf.rs`).
//!
//! Timings are compared with a deliberately generous rule, since CI machines vary: a step fails
//! only when it is more than [`MIN_EXCESS_MS`] slower than its baseline **and** either more
//! than [`REL_FACTOR`] times slower than the run's typical step (the median ratio to the
//! baseline, which absorbs the machine's speed) or more than [`ABS_FACTOR`] times slower than
//! the baseline outright. Debug builds report timings but never fail on them.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A step fails when it is this many times slower than the run's median ratio to the baseline.
pub const REL_FACTOR: f64 = 3.0;
/// A step fails when it is this many times slower than its baseline, whatever the machine.
pub const ABS_FACTOR: f64 = 8.0;
/// Below this excess over the baseline (ms), a step never fails: scheduler noise.
pub const MIN_EXCESS_MS: f64 = 250.0;

/// `tests/golden/bench/<name>`.
pub fn baseline_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/bench").join(name)
}

/// Whether baselines are rewritten (`CADLAB_BLESS=1`).
pub fn blessing() -> bool {
    std::env::var("CADLAB_BLESS").is_ok_and(|v| v == "1")
}

/// Reads a committed baseline (`None` when missing).
pub fn read_baseline<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    let path = baseline_path(name);
    let text = std::fs::read_to_string(&path).ok()?;
    Some(serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

/// Writes a baseline (when blessing).
pub fn write_baseline<T: serde::Serialize>(name: &str, value: &T) {
    let path = baseline_path(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(value).unwrap() + "\n").unwrap();
    eprintln!("blessed {}", path.display());
}

/// Directory of the reports (`$CARGO_TARGET_TMPDIR/bench`).
pub fn report_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("bench");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Writes `<report_dir>/<name>.json` and `<name>.md` (a Markdown summary for the CI job).
pub fn write_report<T: serde::Serialize>(name: &str, value: &T, markdown: &str) -> PathBuf {
    let d = report_dir();
    let json = d.join(format!("{name}.json"));
    std::fs::write(&json, serde_json::to_string_pretty(value).unwrap() + "\n").unwrap();
    std::fs::write(d.join(format!("{name}.md")), markdown).unwrap();
    eprintln!("report: {}", json.display());
    json
}

/// The build profile, recorded in the reports.
pub fn profile() -> &'static str {
    if cfg!(debug_assertions) { "debug" } else { "release" }
}

/// One step's timing against its baseline.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TimingVerdict {
    /// Step name.
    pub name: String,
    /// Measured time (ms).
    pub ms: f64,
    /// Baseline time (ms), if the step has one.
    pub baseline_ms: Option<f64>,
    /// `ms / baseline_ms`.
    pub ratio: Option<f64>,
    /// Why the step fails, if it does.
    pub failure: Option<String>,
}

/// Compares measured step times (ms) with their baseline under the rule in the module docs.
/// Returns one verdict per measured step (in the order given) and the run's median ratio.
/// In debug builds no step fails.
pub fn check_timings(
    measured: &[(String, f64)],
    baseline: &BTreeMap<String, f64>,
) -> (Vec<TimingVerdict>, Option<f64>) {
    check_timings_with(measured, baseline, !cfg!(debug_assertions))
}

/// [`check_timings`], failing steps only when `enforce`.
pub fn check_timings_with(
    measured: &[(String, f64)],
    baseline: &BTreeMap<String, f64>,
    enforce: bool,
) -> (Vec<TimingVerdict>, Option<f64>) {
    let ratio = |ms: f64, b: f64| ms / b.max(1.0);
    let mut ratios: Vec<f64> = measured.iter().filter_map(|(n, ms)| baseline.get(n).map(|b| ratio(*ms, *b))).collect();
    ratios.sort_by(f64::total_cmp);
    let median = (!ratios.is_empty()).then(|| ratios[ratios.len() / 2]);
    let verdicts = measured
        .iter()
        .map(|(name, ms)| {
            let b = baseline.get(name).copied();
            let r = b.map(|b| ratio(*ms, b));
            let failure = match (b, r, median) {
                (Some(b), Some(r), Some(m)) if enforce && ms - b > MIN_EXCESS_MS => {
                    if r > REL_FACTOR * m {
                        Some(format!(
                            "{ms:.0} ms vs {b:.0} ms baseline: {r:.1}× slower, {:.1}× more than the run's median ({m:.2}×)",
                            r / m
                        ))
                    } else if r > ABS_FACTOR {
                        Some(format!("{ms:.0} ms vs {b:.0} ms baseline: {r:.1}× slower (limit {ABS_FACTOR}×)"))
                    } else {
                        None
                    }
                }
                _ => None,
            };
            TimingVerdict { name: name.clone(), ms: *ms, baseline_ms: b, ratio: r, failure }
        })
        .collect();
    (verdicts, median)
}

/// Markdown table of timing verdicts.
pub fn timing_table(verdicts: &[TimingVerdict], median: Option<f64>) -> String {
    let mut s = String::from("| step | time | baseline | ratio | |\n|---|---|---|---|---|\n");
    for v in verdicts {
        s.push_str(&format!(
            "| {} | {:.1} ms | {} | {} | {} |\n",
            v.name,
            v.ms,
            v.baseline_ms.map_or("—".into(), |b| format!("{b:.1} ms")),
            v.ratio.map_or("—".into(), |r| format!("{r:.2}×")),
            v.failure.as_deref().unwrap_or("ok")
        ));
    }
    if let Some(m) = median {
        s.push_str(&format!(
            "\nMedian ratio to the baseline: {m:.2}× (fails above {REL_FACTOR}× the median or {ABS_FACTOR}× \
             outright, when more than {MIN_EXCESS_MS} ms slower; {} build)\n",
            profile()
        ));
    }
    s
}
