//! Timings on the synthetic large board (`tests/common/bigboard.rs`) and the open-source corpus.
//!
//! - `cargo run --release --example bigboard [runs]`: the synthetic board. Every step runs `runs`
//!   times (default 3) with the zone fill cache cleared first and the best time is reported, so
//!   each number is a cold run of that step as a fresh CLI process sees it (except the "fills
//!   cached" steps). Numbers are recorded in `docs/BOARD.md`.
//! - `... --example bigboard corpus [runs] [name...]`: import, zone fill, DRC, render and Gerbers
//!   of the corpus boards in `$CADLAB_CORPUS_DIR` (`scripts/fetch-corpus.sh`).
//! - `... --example bigboard dump <dir> [name...]`: every output (fills, DRC, fab files, renders,
//!   exports) of the synthetic board, two small variants and the corpus boards, one directory
//!   each, to compare byte for byte before and after a change (DECISIONS D23):
//!   `diff -r before after`.

#[path = "../tests/common/bigboard.rs"]
mod bigboard;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("corpus") => {
            let runs: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(3);
            let only: Vec<String> = args.iter().skip(2).cloned().collect();
            if std::env::var_os("CADLAB_CORPUS_DIR").is_none() {
                eprintln!("set CADLAB_CORPUS_DIR (scripts/fetch-corpus.sh)");
                std::process::exit(2);
            }
            for (name, rows) in bigboard::corpus_timings(&only, runs) {
                let summary = rows[0].note.clone();
                print!("{}", bigboard::report(&format!("{name}: {summary}"), &rows));
            }
        }
        Some("dump") => {
            let Some(dir) = args.get(1).map(std::path::PathBuf::from) else {
                eprintln!("usage: bigboard dump <dir> [corpus project...]");
                std::process::exit(2);
            };
            let only: Vec<String> = args.iter().skip(2).cloned().collect();
            let specs = [
                ("synthetic", bigboard::Spec::default()),
                ("small-1", bigboard::Spec::small(1)),
                ("small-2", bigboard::Spec::small(2)),
            ];
            for (name, spec) in specs {
                let (_tmp, r, mut s) = bigboard::build(spec);
                bigboard::dump(&r, &mut s, &dir.join(name));
                eprintln!("{name}");
            }
            for (name, p, _) in bigboard::corpus_boards(&only) {
                let (_tmp, r, mut s) = bigboard::session_with(p);
                bigboard::dump(&r, &mut s, &dir.join(&name));
                eprintln!("{name}");
            }
        }
        _ => {
            let runs: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(3);
            let (summary, rows) = bigboard::timings(bigboard::Spec::default(), runs);
            print!("{}", bigboard::report(&summary, &rows));
        }
    }
}
