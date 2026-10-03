//! Timings on the synthetic large board (`tests/common/bigboard.rs`):
//! `cargo run --release --example bigboard [runs]`.
//!
//! Every step runs `runs` times (default 3) with the zone fill cache cleared first and the best
//! time is reported, so each number is a cold run of that step as a fresh CLI process sees it
//! (except the "fills cached" steps). Numbers are recorded in `docs/BOARD.md`.

#[path = "../tests/common/bigboard.rs"]
mod bigboard;

fn main() {
    let runs: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(3);
    let (summary, rows) = bigboard::timings(bigboard::Spec::default(), runs);
    print!("{}", bigboard::report(&summary, &rows));
}
